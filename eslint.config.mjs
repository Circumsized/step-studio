// Lint, staged in.
//
// This is a 281-file renderer with no static analysis at all, so the temptation is
// to switch on everything and spend a week red. That is how a gate gets bypassed on
// day one. Instead: the rules below are the ones that catch real defects, and the
// set grows only when a rule can be turned on without a mountain of pre-existing
// violations.
//
// What is deliberately NOT here yet, and why:
//   - `@typescript-eslint/no-explicit-any` and friends: hundreds of hits, all
//     pre-existing. Turning it on now produces a red gate nobody can clear.
//   - stylistic rules (quotes, semicolons, member ordering): `prettier`-shaped
//     concerns, and this project has no prettier either. Adding a formatter is a
//     separate decision from adding a linter.
//   - import ordering, file-length, complexity: all real, all noisy on a codebase
//     this size. They go in when there is capacity to clear them.
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import jsxa11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  {
    // Only the renderer and the shared packages are linted. `scripts/` is Node
    // tooling run by `node --test`, and `crates/` is Rust.
    files: ["apps/renderer/src/**/*.{ts,tsx}", "packages/*/src/**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      // The renderer runs in a WebView. Without these, every `window` and
      // `document` is an `no-undef` error, which is a config gap rather than a
      // finding — and a gate that reports 48 phantom errors teaches people to
      // ignore it.
      globals: {
        window: "readonly", document: "readonly", navigator: "readonly",
        location: "readonly", history: "readonly", localStorage: "readonly",
        sessionStorage: "readonly", fetch: "readonly", URL: "readonly",
        URLSearchParams: "readonly", Request: "readonly", Response: "readonly",
        Headers: "readonly", AbortController: "readonly", AbortSignal: "readonly",
        setTimeout: "readonly", clearTimeout: "readonly",
        setInterval: "readonly", clearInterval: "readonly",
        requestAnimationFrame: "readonly", cancelAnimationFrame: "readonly",
        ResizeObserver: "readonly", IntersectionObserver: "readonly",
        MutationObserver: "readonly", MutationRecord: "readonly",
        KeyboardEvent: "readonly", MouseEvent: "readonly", Event: "readonly",
        CustomEvent: "readonly", HTMLElement: "readonly", Element: "readonly",
        Node: "readonly", NodeListOf: "readonly", HTMLInputElement: "readonly",
        HTMLTextAreaElement: "readonly", HTMLDivElement: "readonly",
        HTMLButtonElement: "readonly", HTMLAnchorElement: "readonly",
        SVGElement: "readonly", File: "readonly", FileReader: "readonly",
        Blob: "readonly", FormData: "readonly", structuredClone: "readonly",
        queueMicrotask: "readonly", MessageEvent: "readonly",
        getComputedStyle: "readonly", matchMedia: "readonly",
        process: "readonly", console: "readonly", Buffer: "readonly",
        __dirname: "readonly",
      },
    },
  },
  {
    // Node-side tooling, which is CommonJS/ESM mixed and has no DOM.
    files: ["scripts/**/*.mjs", "*.mjs", "test/**/*.mjs"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: {
        process: "readonly", console: "readonly", Buffer: "readonly",
        URL: "readonly", URLSearchParams: "readonly", fetch: "readonly",
        setTimeout: "readonly", clearTimeout: "readonly",
        setInterval: "readonly", clearInterval: "readonly",
        __dirname: "readonly", __filename: "readonly", module: "readonly",
        require: "readonly", exports: "readonly", global: "readonly",
        structuredClone: "readonly", TextEncoder: "readonly",
        TextDecoder: "readonly", AbortController: "readonly",
        AbortSignal: "readonly", queueMicrotask: "readonly",
      },
    },
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  jsxa11y.flatConfigs.recommended,
  {
    plugins: { "react-hooks": reactHooks },
    rules: {
      // Real defects, not style.
      // Staged: 54 real dead bindings across 35 files — unused imports, unused
      // constants, state that is written and never read. Two of them are more than
      // dead weight and are filed separately: `missingModelSaveError` and
      // `missingRoleLabels` in ModelConfigPage are a complete save-time validation
      // ("Cannot save: role X has no primary model", with i18n) that nothing calls,
      // which means that guard is not enforced. `warn` until the backlog is
      // cleared; `error` after.
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": ["warn", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
      "no-console": "off",
      // `warn`, not `error`: this codebase deliberately narrows dep arrays and
      // says why in a comment at each site (`structureToken` standing in for
      // `rows`). Erroring would turn those into churn; warning keeps them visible.
      // Registered flat-style below because `reactHooks.configs.recommended` is
      // eslintrc-shaped and ESLint 9 rejects it.
      "react-hooks/exhaustive-deps": "warn",
      // `warn`, not `error`: this codebase deliberately narrows dep arrays and
      // says why in a comment at each site (`structureToken` standing in for
      // `rows`). Erroring would turn those into churn; warning keeps them visible.
      "react-hooks/exhaustive-deps": "warn",
      eqeqeq: ["error", "smart"],
      "no-var": "error",
      // Staged. Three of the four were genuine and are fixed; the one left is
      // `runtimeEnsure.ts`'s `timer`, assigned inside a promise executor the rule
      // does not track. `warn` until somebody reads that function and decides
      // which of the two is right.
      "prefer-const": "warn",
      // The frame record/field separators are control characters by design — see
      // `crates/studio-bridge/src/frame.rs`. The rule cannot know that.
      "no-control-regex": "off",

      // `any` is everywhere in this codebase by design (Host payloads are `Value`).
      // Warn rather than error: visible, not blocking.
      "@typescript-eslint/no-explicit-any": "warn",

      // ---- a11y, staged ----
      //
      // 177 violations exist today, across 49 files. Turning them all on as
      // `error` produces a gate nobody can clear, which is how a gate gets
      // bypassed. So the rules with a large pre-existing count start at `warn`:
      // visible in every run, not blocking, and promoted to `error` as the
      // violations are fixed. The counts below are from the run that introduced
      // this file, so the next person can see the direction of travel.
      //
      // Already at `error` — the rules with few or zero hits, including the class
      // this gate was added for. Three `role="button"` spans with `tabIndex={0}`
      // but no `onKeyDown` and no accessible name existed in ModelConfigPage;
      // they are fixed, and these keep them from coming back.
      "jsx-a11y/alt-text": "error",
      "jsx-a11y/aria-props": "error",
      "jsx-a11y/aria-unsupported-elements": "error",
      "jsx-a11y/role-has-required-aria-props": "error",
      "jsx-a11y/role-supports-aria-props": "error",
      // The media panes preview whatever the user generated — a video or audio
      // artifact from a session. There is no caption track to point at, and a
      // `<track>` for content we do not produce would be a lie.
      "jsx-a11y/media-has-caption": "off",
      "jsx-a11y/scope": "error",
      // Focusable `role="separator"` is the ARIA APG splitter pattern, not a
      // violation: the sidebar, git and split-pane resizers carry
      // `aria-valuemin/max/now`, `tabIndex={0}` and an `onKeyDown` that moves
      // them with the arrow keys. `role="tabpanel"` with `tabIndex={0}` is the
      // ARIA APG tabs pattern — focusable so a keyboard user can Tab into it.
      // `role="application"` is the browser pane's live frame, which forwards
      // key, paste and composition events to the remote page. The rule's
      // interactive-element list predates all three. `<pre tabIndex={0}>` is the
      // log viewer, made keyboard-scrollable on purpose.
      // One site left: the skills flyout's `role="option"` sits on a `<div>` with
      // an onClick but no listbox parent and no keyboard path. Making it right
      // means deciding whether the flyout is a listbox at all — a design
      // question, not a lint fix.
      "jsx-a11y/no-noninteractive-element-to-interactive-role": "warn",
      // `facade.ts`'s `#lastInstallResult` is written on both the success and the
      // failure path and never read. Dead state somebody meant to surface —
      // probably on the diagnostics page — and deleting it would delete the
      // intent along with the code.
      "no-unused-private-class-members": "warn",
      "jsx-a11y/no-noninteractive-tabindex": ["error", { roles: ["separator", "tabpanel", "application"], tags: ["pre"] }],
      "jsx-a11y/no-redundant-roles": "error",

      // Staged. Real violations, mostly two idioms: a `<div onClick>` with no role
      // and no keyboard handler, and a `<label>` sitting beside an `<input>`
      // where neither carries an id. Both are WCAG 2.1.1 / 1.3.1 defects, not
      // rule noise — the rule cannot see that a custom `Switch` renders a native
      // `<button role="switch">`, but it can see a label with nothing pointing at
      // it.
      "jsx-a11y/label-has-associated-control": "warn",        // 37
      "jsx-a11y/no-noninteractive-element-interactions": "warn", // 37
      "jsx-a11y/interactive-supports-focus": "warn",          // 20
      "jsx-a11y/no-static-element-interactions": "warn",      // 19
      "jsx-a11y/click-events-have-key-events": "warn",        // 19
      // Deliberate in this codebase: the file-search box, the remote-plan dialog
      // and the composer all focus their input on open. That is focus management,
      // not an oversight, and the rule cannot tell the two apart.
      "jsx-a11y/no-autofocus": "off",                        // 23, intentional
    },
  },
  {
    // Screenshot capture scripts are *injected into a page*, so they run with a
    // DOM and no Node globals — the opposite of the tooling block above.
    // These drive a real page: a headless browser evaluates their source, or
    // they are loaded into one to measure it. `window` and the timers are the
    // page's, not Node's.
    files: [
      "scripts/capture-*.mjs",
      "scripts/render-work-bench.mjs",
      "scripts/streaming-perf-gate.mjs",
      "scripts/native-visibility-smoke.cjs",
    ],
    languageOptions: {
      globals: {
        document: "readonly", window: "readonly", navigator: "readonly",
        location: "readonly", fetch: "readonly", console: "readonly",
        setTimeout: "readonly", clearTimeout: "readonly",
        MutationObserver: "readonly", requestAnimationFrame: "readonly",
        getComputedStyle: "readonly", Node: "readonly",
        setTimeout: "readonly", clearTimeout: "readonly",
        setInterval: "readonly", clearInterval: "readonly",
        performance: "readonly",
      },
    },
  },
  {
    // CommonJS tooling. `.cjs` stays CommonJS on purpose: it is loaded by a
    // harness that predates the ESM migration, and `@typescript-eslint`'s
    // `no-require-imports` does not know that.
    files: ["**/*.cjs"],
    languageOptions: {
      sourceType: "commonjs",
      globals: {
        require: "readonly", module: "readonly", exports: "readonly",
        __dirname: "readonly", __filename: "readonly",
        process: "readonly", console: "readonly", Buffer: "readonly",
      },
    },
    rules: { "@typescript-eslint/no-require-imports": "off" },
  },
  {
    // An AudioWorklet processor runs in `AudioWorkletGlobalScope`, which has
    // neither `window` nor `document`. Without these two globals the file is
    // pure `no-undef` noise.
    files: ["**/*.worklet.js"],
    languageOptions: {
      globals: {
        AudioWorkletProcessor: "readonly",
        registerProcessor: "readonly",
        sampleRate: "readonly",
        currentTime: "readonly",
      },
    },
  },
  {
    // Tests may assert on anything, including unused fixtures.
    files: ["**/*.test.{ts,tsx}", "**/test/**"],
    rules: {
      "@typescript-eslint/no-unused-vars": "off",
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
);
