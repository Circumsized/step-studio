import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { detectSystemLanguage, formatTranslation, resolveLanguage, translate } from "./I18nContext";

describe("i18n system tests", () => {
  const originalNavigator = globalThis.navigator;

  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    Object.defineProperty(globalThis, "navigator", {
      value: originalNavigator,
      configurable: true,
      writable: true,
    });
  });

  it("detects Chinese when navigator.language starts with zh", () => {
    Object.defineProperty(globalThis, "navigator", {
      value: { language: "zh-CN", languages: ["zh-CN", "zh"] },
      configurable: true,
      writable: true,
    });
    expect(detectSystemLanguage()).toBe("zh");
    expect(resolveLanguage("system")).toBe("zh");
  });

  it("detects English when navigator.language is English or other", () => {
    Object.defineProperty(globalThis, "navigator", {
      value: { language: "en-US", languages: ["en-US", "en"] },
      configurable: true,
      writable: true,
    });
    expect(detectSystemLanguage()).toBe("en");
    expect(resolveLanguage("system")).toBe("en");

    Object.defineProperty(globalThis, "navigator", {
      value: { language: "ja-JP", languages: ["ja-JP"] },
      configurable: true,
      writable: true,
    });
    expect(detectSystemLanguage()).toBe("en");
  });

  it("explicit language setting overrides system language", () => {
    Object.defineProperty(globalThis, "navigator", {
      value: { language: "zh-CN" },
      configurable: true,
      writable: true,
    });
    expect(resolveLanguage("en")).toBe("en");
    expect(resolveLanguage("zh")).toBe("zh");

    Object.defineProperty(globalThis, "navigator", {
      value: { language: "en-US" },
      configurable: true,
      writable: true,
    });
    expect(resolveLanguage("zh")).toBe("zh");
    expect(resolveLanguage("en")).toBe("en");
  });

  it("formats template parameters properly", () => {
    expect(formatTranslation("Hello {name}, you have {count} items", { name: "Alice", count: 3 })).toBe(
      "Hello Alice, you have 3 items",
    );
    expect(formatTranslation("No params")).toBe("No params");
  });

  it("translates keys for Chinese and English", () => {
    expect(translate("zh", "common.save")).toBe("保存");
    expect(translate("en", "common.save")).toBe("Save");
    expect(translate("zh", "settings.general.title")).toBe("常规");
    expect(translate("en", "settings.general.title")).toBe("General");
    expect(translate("zh", "history.messagesCount", { count: 5 })).toBe("5 条消息");
    expect(translate("en", "history.messagesCount", { count: 5 })).toBe("5 messages");
  });

  it("gracefully falls back when key is missing", () => {
    expect(translate("zh", "nonexistent.key.path")).toBe("nonexistent.key.path");
  });

  it("returns a non-string key unchanged instead of throwing", () => {
    // `translate` is exported and typed `key: string`, but the type is a
    // compile-time promise: Host data reaching it can be `undefined`. The
    // `typeof path !== "string"` guard in `lookupKey` is what keeps
    // `path.split(".")` from throwing. No in-module caller gets here —
    // `labelFor` substitutes the raw key with `?? key` — so this is the only
    // thing that pins the guard.
    expect(() => translate("zh", undefined as never)).not.toThrow();
    expect(translate("zh", undefined as never)).toBeUndefined();
    expect(translate("en", 42 as never)).toBe(42);
  });
});
