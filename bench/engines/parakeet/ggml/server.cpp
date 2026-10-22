// Long-lived Parakeet TDT server for the Wordfall bench external engine protocol
// (see bench/README.md). Uses whisper.cpp's include/parakeet.h directly; the
// model is loaded once and every stdin line is one clip.
//
//   parakeet-server --model ggml-parakeet-tdt-0.6b-v3-f16.bin [--threads N] [--no-gpu] [--name NAME]
//
// stdin : {"id": "...", "wav": "/abs/path.wav", "context": [...]}   (context is ignored: greedy TDT only)
// stdout: {"id": "...", "text": "...", "ms": 123}  |  {"id": "...", "error": "..."}
// Logs go to stderr.

#include "parakeet.h"

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

namespace {

// ---------- minimal JSON helpers (flat objects with string values are all we need) ----------

std::string json_escape(const std::string & s) {
    std::string out;
    out.reserve(s.size() + 8);
    for (unsigned char c : s) {
        switch (c) {
            case '"':  out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n";  break;
            case '\r': out += "\\r";  break;
            case '\t': out += "\\t";  break;
            default:
                if (c < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof(buf), "\\u%04x", c);
                    out += buf;
                } else {
                    out += (char) c;
                }
        }
    }
    return out;
}

void append_utf8(std::string & out, uint32_t cp) {
    if (cp < 0x80) {
        out += (char) cp;
    } else if (cp < 0x800) {
        out += (char) (0xC0 | (cp >> 6));
        out += (char) (0x80 | (cp & 0x3F));
    } else if (cp < 0x10000) {
        out += (char) (0xE0 | (cp >> 12));
        out += (char) (0x80 | ((cp >> 6) & 0x3F));
        out += (char) (0x80 | (cp & 0x3F));
    } else {
        out += (char) (0xF0 | (cp >> 18));
        out += (char) (0x80 | ((cp >> 12) & 0x3F));
        out += (char) (0x80 | ((cp >> 6) & 0x3F));
        out += (char) (0x80 | (cp & 0x3F));
    }
}

// Parses a JSON string literal starting at s[i] == '"'. Advances i past the closing quote.
bool parse_json_string(const std::string & s, size_t & i, std::string & out) {
    if (i >= s.size() || s[i] != '"') return false;
    ++i;
    out.clear();
    while (i < s.size()) {
        char c = s[i++];
        if (c == '"') return true;
        if (c != '\\') { out += c; continue; }
        if (i >= s.size()) return false;
        char e = s[i++];
        switch (e) {
            case '"': out += '"'; break;
            case '\\': out += '\\'; break;
            case '/': out += '/'; break;
            case 'b': out += '\b'; break;
            case 'f': out += '\f'; break;
            case 'n': out += '\n'; break;
            case 'r': out += '\r'; break;
            case 't': out += '\t'; break;
            case 'u': {
                if (i + 4 > s.size()) return false;
                uint32_t cp = (uint32_t) std::stoul(s.substr(i, 4), nullptr, 16);
                i += 4;
                if (cp >= 0xD800 && cp <= 0xDBFF && i + 6 <= s.size() && s[i] == '\\' && s[i + 1] == 'u') {
                    uint32_t lo = (uint32_t) std::stoul(s.substr(i + 2, 4), nullptr, 16);
                    if (lo >= 0xDC00 && lo <= 0xDFFF) {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        i += 6;
                    }
                }
                append_utf8(out, cp);
                break;
            }
            default: return false;
        }
    }
    return false;
}

// Finds top-level "key": "<string>" in a flat JSON object. Good enough for the bench protocol.
bool json_get_string(const std::string & line, const std::string & key, std::string & out) {
    const std::string needle = "\"" + key + "\"";
    size_t pos = 0;
    while ((pos = line.find(needle, pos)) != std::string::npos) {
        size_t i = pos + needle.size();
        while (i < line.size() && isspace((unsigned char) line[i])) ++i;
        if (i < line.size() && line[i] == ':') {
            ++i;
            while (i < line.size() && isspace((unsigned char) line[i])) ++i;
            return parse_json_string(line, i, out);
        }
        pos = i;
    }
    return false;
}

// ---------- WAV reader: PCM16 / float32, any channel count (downmixed), must be 16 kHz ----------

bool read_wav(const std::string & path, std::vector<float> & pcm, std::string & err) {
    std::ifstream f(path, std::ios::binary);
    if (!f) { err = "cannot open wav: " + path; return false; }
    std::vector<char> buf((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    if (buf.size() < 12 || memcmp(buf.data(), "RIFF", 4) != 0 || memcmp(buf.data() + 8, "WAVE", 4) != 0) {
        err = "not a RIFF/WAVE file"; return false;
    }
    auto u16 = [&](size_t o) { return (uint16_t) ((uint8_t) buf[o] | ((uint8_t) buf[o + 1] << 8)); };
    auto u32 = [&](size_t o) { return (uint32_t) u16(o) | ((uint32_t) u16(o + 2) << 16); };

    uint16_t fmt = 0, channels = 0, bits = 0;
    uint32_t rate = 0;
    size_t data_off = 0, data_len = 0;
    for (size_t o = 12; o + 8 <= buf.size();) {
        uint32_t len = u32(o + 4);
        if (memcmp(buf.data() + o, "fmt ", 4) == 0 && o + 8 + 16 <= buf.size()) {
            fmt = u16(o + 8); channels = u16(o + 10); rate = u32(o + 12); bits = u16(o + 22);
            if (fmt == 0xFFFE && len >= 26) fmt = u16(o + 8 + 24);  // WAVE_FORMAT_EXTENSIBLE subformat
        } else if (memcmp(buf.data() + o, "data", 4) == 0) {
            data_off = o + 8;
            data_len = std::min<size_t>(len, buf.size() - data_off);
            break;
        }
        o += 8 + len + (len & 1);
    }
    if (!data_off || !channels) { err = "wav: missing fmt/data chunk"; return false; }
    if (rate != PARAKEET_SAMPLE_RATE) { err = "wav: expected 16000 Hz, got " + std::to_string(rate); return false; }

    const size_t bytes_per = bits / 8;
    const size_t frames = data_len / (bytes_per * channels);
    pcm.assign(frames, 0.0f);
    for (size_t n = 0; n < frames; ++n) {
        float acc = 0.0f;
        for (size_t c = 0; c < channels; ++c) {
            const char * p = buf.data() + data_off + (n * channels + c) * bytes_per;
            if (fmt == 1 && bits == 16) {
                int16_t v; memcpy(&v, p, 2); acc += v / 32768.0f;
            } else if (fmt == 3 && bits == 32) {
                float v; memcpy(&v, p, 4); acc += v;
            } else {
                err = "wav: unsupported sample format (need PCM16 or float32)"; return false;
            }
        }
        pcm[n] = acc / channels;
    }
    return true;
}

double now_ms() {
    using namespace std::chrono;
    return duration<double, std::milli>(steady_clock::now().time_since_epoch()).count();
}

bool transcribe(parakeet_context * ctx, const std::vector<float> & pcm, int n_threads, std::string & text) {
    parakeet_full_params p = parakeet_full_default_params(PARAKEET_SAMPLING_GREEDY);
    p.n_threads = n_threads;
    p.no_context = true;
    if (parakeet_full(ctx, p, pcm.data(), (int) pcm.size()) != 0) return false;
    text.clear();
    const int n = parakeet_full_n_segments(ctx);
    for (int i = 0; i < n; ++i) {
        const char * seg = parakeet_full_get_segment_text(ctx, i);
        if (!seg) continue;
        if (!text.empty() && *seg != ' ') text += ' ';
        text += seg;
    }
    // trim
    size_t a = text.find_first_not_of(" \t\n");
    size_t b = text.find_last_not_of(" \t\n");
    text = (a == std::string::npos) ? std::string() : text.substr(a, b - a + 1);
    return true;
}

void emit(const std::string & line) {
    fwrite(line.data(), 1, line.size(), stdout);
    fputc('\n', stdout);
    fflush(stdout);
}

}  // namespace

int main(int argc, char ** argv) {
    std::string model, name;
    int n_threads = 4;
    bool use_gpu = true;
    for (int i = 1; i < argc; ++i) {
        std::string a = argv[i];
        if ((a == "--model" || a == "-m") && i + 1 < argc) model = argv[++i];
        else if ((a == "--threads" || a == "-t") && i + 1 < argc) n_threads = std::stoi(argv[++i]);
        else if (a == "--no-gpu") use_gpu = false;
        else if (a == "--name" && i + 1 < argc) name = argv[++i];
        else { fprintf(stderr, "unknown argument: %s\n", a.c_str()); return 2; }
    }
    if (model.empty()) { fprintf(stderr, "usage: %s --model FILE [--threads N] [--no-gpu] [--name NAME]\n", argv[0]); return 2; }
    if (name.empty()) {
        size_t s = model.find_last_of('/');
        name = s == std::string::npos ? model : model.substr(s + 1);
    }

    const double t0 = now_ms();
    parakeet_context_params cp = parakeet_context_default_params();
    cp.use_gpu = use_gpu;
    parakeet_context * ctx = parakeet_init_from_file_with_params(model.c_str(), cp);
    if (!ctx) { fprintf(stderr, "failed to load model %s\n", model.c_str()); return 1; }

    // Warm-up on 1 s of silence (allocates graphs / compiles Metal pipelines).
    {
        std::vector<float> silence(PARAKEET_SAMPLE_RATE, 0.0f);
        std::string ignored;
        if (!transcribe(ctx, silence, n_threads, ignored)) fprintf(stderr, "warm-up failed\n");
    }
    const long load_ms = (long) (now_ms() - t0);
    fprintf(stderr, "parakeet-server: %s\n", parakeet_print_system_info());

    emit(std::string("{\"ready\": true, \"engine\": \"parakeet-ggml\", \"model\": \"") + json_escape(name) +
         "\", \"loadMs\": " + std::to_string(load_ms) + "}");

    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.find_first_not_of(" \t\r") == std::string::npos) continue;
        std::string id, wav;
        json_get_string(line, "id", id);
        if (!json_get_string(line, "wav", wav)) {
            emit("{\"id\": \"" + json_escape(id) + "\", \"error\": \"missing wav\"}");
            continue;
        }
        std::vector<float> pcm;
        std::string err;
        if (!read_wav(wav, pcm, err)) {
            emit("{\"id\": \"" + json_escape(id) + "\", \"error\": \"" + json_escape(err) + "\"}");
            continue;
        }
        if (pcm.size() < (size_t) PARAKEET_HOP_LENGTH * 2) {
            emit("{\"id\": \"" + json_escape(id) + "\", \"text\": \"\", \"ms\": 0}");
            continue;
        }
        std::string text;
        const double s = now_ms();
        const bool ok = transcribe(ctx, pcm, n_threads, text);
        const long ms = (long) (now_ms() - s);
        if (!ok) {
            emit("{\"id\": \"" + json_escape(id) + "\", \"error\": \"parakeet_full failed\"}");
        } else {
            emit("{\"id\": \"" + json_escape(id) + "\", \"text\": \"" + json_escape(text) + "\", \"ms\": " + std::to_string(ms) + "}");
        }
    }
    parakeet_free(ctx);
    return 0;
}
