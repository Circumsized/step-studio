import { RenderCache, renderBytes } from "./renderCache";

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const nested of Object.values(value)) deepFreeze(nested);
  }
  return value;
}

export const MERMAID_CONFIG = deepFreeze({
  startOnLoad: false,
  securityLevel: "strict" as const,
  theme: "default" as const,
  // `strict` makes mermaid run its output through DOMPurify and drop HTML
  // labels, event handlers and non-relative hrefs. It is also mermaid's own
  // default, and mermaid's default `secure` list already contains
  // `securityLevel`, so a diagram cannot talk its way out of it.
  //
  // The two settings that actually change something:
  //
  // `htmlLabels: false` — `getEffectiveHtmlLabels` resolves
  // `config.htmlLabels ?? config.flowchart.htmlLabels ?? true`, so HTML labels
  // (rendered through `foreignObject`) are ON unless turned off. Node labels
  // become SVG text instead of HTML.
  //
  // `secure: ["htmlLabels"]` — mermaid lets a diagram's own frontmatter override
  // `initialize()`, and `sanitize` drops only the keys listed in
  // `["secure", ...siteConfig.secure]` from those directives. Without this entry
  // a fenced block carrying `htmlLabels: true` re-opens the HTML-label path on a
  // per-diagram basis, which is exactly the thing `htmlLabels: false` just
  // closed. `securityLevel` is deliberately not repeated: it is already in
  // mermaid's default list, and `assignWithDepth` unions arrays rather than
  // replacing them, so the default protections all survive.
  //
  // Not `as const`: mermaid's `secure` is `string[]`, and a readonly tuple is
  // not assignable to it. The array itself is frozen by `deepFreeze` below.
  htmlLabels: false,
  secure: ["htmlLabels"],
});
export type MermaidConfig = typeof MERMAID_CONFIG;
export function mermaidBudget(code: string): { allowed: boolean; score: number; reason?: string } {
  if (code.length > 20_000) return { allowed: false, score: 0, reason: "图表过大，已保留源码" };
  const lines = code.split("\n").length;
  // A cheap heuristic (lines + edge features + node features), not a parser
  // or an estimate of the graph algorithm's exact complexity.
  const score = lines + (code.match(/-->|==>|-.->|->>|--|->/g)?.length ?? 0) + (code.match(/[[({]/g)?.length ?? 0);
  if (lines > 600 || score > 1500) return { allowed: false, score, reason: "图表较复杂，已保留源码" };
  return { allowed: true, score };
}
type Subscriber = { active: () => boolean; result: (svg: string | null) => void };
type Job = { key: string; code: string; config: MermaidConfig; subscribers: Set<Subscriber> };
export class MermaidQueue {
  readonly #cache = new RenderCache<string>(32, 4 * 1024 * 1024);
  readonly #waiting = new Map<string, Job>();
  readonly #slots = new Set<() => void>();
  #running: Job | undefined;
  #disposed = false;
  #notifyPending = false;
  constructor(readonly options: { render: (code: string, config: MermaidConfig, active: () => boolean) => Promise<string | undefined>; hidden: () => boolean }) {}

  onSlot(callback: () => void): () => void { this.#slots.add(callback); return () => { this.#slots.delete(callback); }; }
  request(code: string, subscriber: Subscriber, config = MERMAID_CONFIG): { status: "accepted" | "full" | "rejected"; cancel: () => void } {
    const empty = () => {};
    if (this.#disposed || !subscriber.active() || !mermaidBudget(code).allowed) return { status: "rejected", cancel: empty };
    const key = JSON.stringify(config) + "\n" + code;
    const cached = this.#cache.get(key);
    if (cached !== undefined) {
      let cancelled = false;
      queueMicrotask(() => { if (!cancelled && !this.#disposed && subscriber.active()) subscriber.result(cached); });
      return { status: "accepted", cancel: () => { cancelled = true; } };
    }
    let job = this.#running?.key === key ? this.#running : this.#waiting.get(key);
    if (job === undefined) {
      // Reserve a slot for a render deferred by a visibility change during
      // lazy module loading. Total pending work never exceeds eight jobs.
      if (this.#waiting.size + (this.#running === undefined ? 0 : 1) >= 8) return { status: "full", cancel: empty };
      job = { key, code, config, subscribers: new Set() };
      this.#waiting.set(key, job);
    }
    job.subscribers.add(subscriber);
    this.pump();
    return { status: "accepted", cancel: () => {
      job.subscribers.delete(subscriber);
      if (job.subscribers.size === 0 && this.#waiting.get(key) === job) { this.#waiting.delete(key); this.#notify(); }
    } };
  }

  #notify(): void {
    if (this.#notifyPending || this.#disposed) return;
    this.#notifyPending = true;
    queueMicrotask(() => {
      this.#notifyPending = false;
      if (!this.#disposed) for (const callback of [...this.#slots]) callback();
    });
  }

  pump(): void {
    if (this.#disposed || this.#running !== undefined || this.options.hidden()) return;
    for (const [key, job] of this.#waiting) {
      for (const subscriber of job.subscribers) if (!subscriber.active()) job.subscribers.delete(subscriber);
      this.#waiting.delete(key);
      if (job.subscribers.size === 0 || !mermaidBudget(job.code).allowed) { this.#notify(); continue; }
      this.#running = job;
      this.#notify();
      let deferred = false;
      const active = () => !this.#disposed && !this.options.hidden() && [...job.subscribers].some((s) => s.active());
      // Never release the running slot before the real render has settled.
      void Promise.resolve().then(() => {
        if (!active()) { deferred = true; return undefined; }
        return this.options.render(job.code, job.config, active);
      }).then((svg) => {
        if (this.#disposed) return;
        if (svg === undefined) { deferred = true; return; }
        const live = [...job.subscribers].filter((s) => s.active());
        if (live.length > 0) this.#cache.put(key, svg, renderBytes(svg));
        for (const subscriber of live) subscriber.result(svg);
      }, () => {
        if (!this.#disposed) for (const subscriber of job.subscribers) if (subscriber.active()) subscriber.result(null);
      }).finally(() => {
        // A page can become hidden between dequeue and execution. Requeue only
        // the still subscribed job, inside the existing eight-entry budget.
        if (!this.#disposed && deferred && [...job.subscribers].some((subscriber) => subscriber.active())) this.#waiting.set(key, job);
        else job.subscribers.clear();
        this.#running = undefined;
        this.#notify(); this.pump();
      });
      break;
    }
  }
  getDiagnostics(): Readonly<Record<string, number>> {
    return { cacheEntries: this.#cache.size, cacheBytes: this.#cache.bytes, queued: this.#waiting.size,
      running: this.#running === undefined ? 0 : 1, slotListeners: this.#slots.size };
  }
  dispose(): void {
    this.#disposed = true;
    this.#running?.subscribers.clear();
    for (const job of this.#waiting.values()) job.subscribers.clear();
    this.#waiting.clear(); this.#cache.clear(); this.#slots.clear();
  }
}
