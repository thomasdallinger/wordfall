// The small Markdown subset meeting notes use: headings, paragraphs, bullet and numbered lists,
// task items ("- [ ]") and **bold** / *italic*. Everything is escaped first, so model output can
// never inject markup.

function escape(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function inline(s: string): string {
  return escape(s)
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*\w])\*(?!\s)(.+?)\*(?!\w)/g, '$1<em>$2</em>');
}

export interface Rendered {
  html: string;
  /** Text of the first "# " heading, which the meeting header shows instead. */
  title: string | null;
}

/** Renders notes; the first top-level heading is returned separately, not rendered. */
export function renderNotes(md: string): Rendered {
  const out: string[] = [];
  let list: 'ul' | 'ol' | null = null;
  let para: string[] = [];
  let title: string | null = null;

  const flushPara = () => {
    if (para.length) out.push(`<p>${para.map(inline).join(' ')}</p>`);
    para = [];
  };
  const closeList = () => {
    if (list) out.push(`</${list}>`);
    list = null;
  };
  const openList = (kind: 'ul' | 'ol') => {
    if (list === kind) return;
    closeList();
    out.push(`<${kind}>`);
    list = kind;
  };

  for (const raw of md.replace(/\r/g, '').split('\n')) {
    const line = raw.trimEnd();
    const trimmed = line.trim();
    if (!trimmed) {
      flushPara();
      closeList();
      continue;
    }
    const heading = /^(#{1,4})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushPara();
      closeList();
      const level = heading[1].length;
      const text = heading[2].replace(/^\*\*(.*)\*\*$/, '$1');
      if (level === 1 && title === null) {
        title = text;
        continue;
      }
      const tag = level <= 2 ? 'h3' : 'h4';
      out.push(`<${tag}>${inline(text)}</${tag}>`);
      continue;
    }
    const task = /^[-*]\s+\[([ xX])\]\s+(.*)$/.exec(trimmed);
    if (task) {
      flushPara();
      openList('ul');
      const done = task[1] !== ' ';
      out.push(`<li class="task${done ? ' done' : ''}"><span class="box" aria-hidden="true"></span><span>${inline(task[2])}</span></li>`);
      continue;
    }
    const bullet = /^[-*•]\s+(.*)$/.exec(trimmed);
    if (bullet) {
      flushPara();
      openList('ul');
      out.push(`<li>${inline(bullet[1])}</li>`);
      continue;
    }
    const numbered = /^\d+[.)]\s+(.*)$/.exec(trimmed);
    if (numbered) {
      flushPara();
      openList('ol');
      out.push(`<li>${inline(numbered[1])}</li>`);
      continue;
    }
    closeList();
    para.push(trimmed);
  }
  flushPara();
  closeList();
  return { html: out.join('\n'), title };
}

/** Notes as plain text for the clipboard: Markdown without the formatting marks. */
export function notesAsText(md: string): string {
  return md
    .replace(/\r/g, '')
    .split('\n')
    .map((l) =>
      l
        .replace(/^#{1,4}\s+/, '')
        .replace(/^(\s*)[-*]\s+\[[ xX]\]\s+/, '$1☐ ')
        .replace(/^(\s*)[-*]\s+/, '$1• ')
        .replace(/\*\*(.+?)\*\*/g, '$1'),
    )
    .join('\n')
    .trim();
}
