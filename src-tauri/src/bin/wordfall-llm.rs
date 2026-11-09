//! `wordfall-llm`: the local language model that writes meeting summaries, as a helper process.
//!
//! llama.cpp ships its own copy of ggml, and whisper.cpp (linked into Wordfall itself) ships
//! another, of a different version. Both export the same C symbols, so they can't share a
//! process. Running the model here also means its memory (several GB) is handed back to the
//! system as soon as a summary is done, and a crash can't take dictation down with it.
//!
//! Protocol: one JSON object per line on stdin and stdout.
//!
//!   → {"op":"load","model":"<path.gguf>","ctx":32768}
//!   ← {"event":"ready","ctx":32768,"device":"NVIDIA GeForce RTX 5070 Ti","gpuLayers":-1}
//!   → {"op":"count","id":1,"text":"…"}
//!   ← {"event":"count","id":1,"tokens":123}
//!   → {"op":"chat","id":2,"messages":[{"role":"system","content":"…"},…],"maxTokens":1500}
//!      (optional: "temperature":0.4, "grammar":"<GBNF>" to force the answer's form)
//!   ← {"event":"progress","id":2,"done":2048,"total":9000}      (reading the prompt)
//!   ← {"event":"token","id":2,"text":"…"}                       (writing)
//!   ← {"event":"done","id":2,"text":"…","promptTokens":9000,"tokens":812,"ms":15012}
//!   ← {"event":"error","id":2,"message":"…"}
//!
//! The process exits when stdin closes. llama.cpp's own log goes to stderr.

use std::{
    ffi::CString,
    io::{BufRead, Write},
    num::NonZeroU32,
    pin::pin,
    time::Instant,
};

use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, LlamaChatMessage, LlamaModel},
    sampling::LlamaSampler,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    Load {
        model: String,
        ctx: u32,
    },
    Count {
        id: u64,
        text: String,
    },
    Chat {
        id: u64,
        messages: Vec<Message>,
        #[serde(rename = "maxTokens")]
        max_tokens: u32,
        #[serde(default)]
        temperature: Option<f32>,
        /// GBNF grammar (root rule `root`) the answer must follow.
        #[serde(default)]
        grammar: Option<String>,
    },
}

#[derive(Deserialize)]
struct Message {
    role: String,
    content: String,
}

/// Prompt tokens decoded per step; also the granularity of progress reports.
const BATCH: u32 = 1024;
/// Free memory left on each device, so dictation (whisper, ~1 GB) keeps working meanwhile.
const MEMORY_MARGIN: usize = 1536 << 20;
/// Smallest context worth running with when memory is tight.
const MIN_CTX: u32 = 8192;

fn send(v: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    // Nobody is listening any more (Wordfall quit): stop instead of finishing the answer.
    if writeln!(out, "{v}").and_then(|_| out.flush()).is_err() {
        std::process::exit(0);
    }
}

fn main() {
    if let Err(e) = run() {
        send(json!({ "event": "error", "id": null, "message": format!("{e:#}") }));
        std::process::exit(1);
    }
}

struct Loaded {
    model: LlamaModel,
    ctx_size: u32,
}

fn run() -> anyhow::Result<()> {
    let backend = LlamaBackend::init()?;
    let mut loaded: Option<Loaded> = None;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                send(json!({ "event": "error", "id": null, "message": format!("bad request: {e}") }));
                continue;
            }
        };
        match req {
            Request::Load { model, ctx } => {
                let started = Instant::now();
                let l = load(&backend, &model, ctx)?;
                eprintln!("wordfall-llm: loaded {model} in {:?}, context {}", started.elapsed(), l.ctx_size);
                send(json!({ "event": "ready", "ctx": l.ctx_size, "device": device_name() }));
                loaded = Some(l);
            }
            Request::Count { id, text } => {
                let Some(l) = loaded.as_ref() else { anyhow::bail!("no model loaded") };
                let n = l.model.vocab().tokenize(text.as_bytes(), false, false).len();
                send(json!({ "event": "count", "id": id, "tokens": n }));
            }
            Request::Chat { id, messages, max_tokens, temperature, grammar } => {
                let Some(l) = loaded.as_ref() else { anyhow::bail!("no model loaded") };
                if let Err(e) = chat(&backend, l, id, &messages, max_tokens, temperature.unwrap_or(0.4), grammar.as_deref()) {
                    send(json!({ "event": "error", "id": id, "message": format!("{e:#}") }));
                }
            }
        }
    }
    Ok(())
}

fn context_params(n_ctx: u32) -> LlamaContextParams {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8) as i32;
    LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(BATCH)
        .with_n_ubatch(512)
        .with_n_threads(threads)
        .with_n_threads_batch(threads)
}

fn load(backend: &LlamaBackend, path: &str, want_ctx: u32) -> anyhow::Result<Loaded> {
    let mut mparams = pin!(LlamaModelParams::default());
    let mut cparams = context_params(want_ctx);
    // Offload as much as fits on the GPU, shrinking the context first if it doesn't.
    let cpath = CString::new(path)?;
    let mut margins = vec![MEMORY_MARGIN; unsafe { llama_cpp_sys_2::llama_max_devices() }];
    match mparams.as_mut().fit_params(&cpath, &mut cparams, &mut margins, MIN_CTX.min(want_ctx), 3) {
        Ok(fit) => eprintln!("wordfall-llm: fitted to memory, context {}", fit.n_ctx),
        Err(e) => eprintln!("wordfall-llm: could not fit to memory ({e}), using defaults"),
    }
    let ctx_size = cparams.n_ctx().map(NonZeroU32::get).unwrap_or(want_ctx);
    let model = LlamaModel::load_from_file(backend, path, &mparams)?;
    let ctx_size = ctx_size.min(model.n_ctx_train().max(MIN_CTX));
    Ok(Loaded { model, ctx_size })
}

fn chat(
    backend: &LlamaBackend,
    l: &Loaded,
    id: u64,
    messages: &[Message],
    max_tokens: u32,
    temperature: f32,
    grammar: Option<&str>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let model = &l.model;
    let template = model.chat_template(None)?;
    let chat: Vec<LlamaChatMessage> = messages
        .iter()
        .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
        .collect::<Result<_, _>>()?;
    let prompt = model.apply_chat_template(&template, &chat, true)?;
    let vocab = model.vocab();
    let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
    let total = tokens.len() as u32;
    anyhow::ensure!(
        total + max_tokens <= l.ctx_size,
        "the prompt ({total} tokens) and answer ({max_tokens}) don't fit the context ({})",
        l.ctx_size
    );

    // A fresh context per request: a clean KV cache, and memory is released in between.
    let mut ctx = model.new_context(backend, context_params(l.ctx_size))?;
    let mut batch = LlamaBatch::new(BATCH as usize, 1);
    let mut done = 0usize;
    for chunk in tokens.chunks(BATCH as usize) {
        batch.clear();
        for (i, t) in chunk.iter().enumerate() {
            let pos = done + i;
            batch.add(*t, pos as i32, &[0], pos + 1 == tokens.len())?;
        }
        ctx.decode(&mut batch)?;
        done += chunk.len();
        send(json!({ "event": "progress", "id": id, "done": done, "total": total }));
    }
    let prompt_ms = started.elapsed().as_millis();

    let mut samplers = Vec::new();
    if let Some(g) = grammar {
        samplers.push(LlamaSampler::grammar(model, g, "root").map_err(|e| anyhow::anyhow!("bad grammar: {e}"))?);
    }
    samplers.extend([
        LlamaSampler::penalties(model.n_vocab(), 256, 1.05, 0.0, 0.0),
        LlamaSampler::top_k(40),
        LlamaSampler::top_p(0.9, 1),
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(temperature),
        LlamaSampler::dist(0x5EED),
    ]);
    let mut sampler = LlamaSampler::chain_simple(samplers);
    let mut text = String::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut pos = tokens.len() as i32;
    let mut generated = 0u32;
    let mut idx = batch.n_tokens() - 1;
    while generated < max_tokens {
        let token = sampler.sample(&ctx, idx);
        if vocab.is_eog(token) {
            break;
        }
        generated += 1;
        pending.extend(vocab.token_to_piece(token, false, None));
        // Tokens can end mid-character; only pass on complete UTF-8.
        let valid = match std::str::from_utf8(&pending) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid > 0 {
            let piece = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            text.push_str(&piece);
            send(json!({ "event": "token", "id": id, "text": piece }));
        }
        batch.clear();
        batch.add(token, pos, &[0], true)?;
        pos += 1;
        idx = 0;
        ctx.decode(&mut batch)?;
    }
    eprintln!(
        "wordfall-llm: request {id}: {total} prompt tokens in {prompt_ms} ms, {generated} tokens in {} ms",
        started.elapsed().as_millis() - prompt_ms
    );
    send(json!({
        "event": "done", "id": id, "text": text, "promptTokens": total, "tokens": generated,
        "ms": started.elapsed().as_millis() as u64,
    }));
    Ok(())
}

/// The first GPU llama.cpp sees, for the log.
fn device_name() -> String {
    use llama_cpp_sys_2 as sys;
    use std::ffi::CStr;
    // SAFETY: ggml's backend registry lives for the whole process and owns the returned strings.
    unsafe {
        for i in 0..sys::ggml_backend_dev_count() {
            let dev = sys::ggml_backend_dev_get(i);
            let kind = sys::ggml_backend_dev_type(dev);
            if kind == sys::GGML_BACKEND_DEVICE_TYPE_GPU || kind == sys::GGML_BACKEND_DEVICE_TYPE_IGPU {
                return CStr::from_ptr(sys::ggml_backend_dev_description(dev)).to_string_lossy().into_owned();
            }
        }
    }
    "CPU".into()
}
