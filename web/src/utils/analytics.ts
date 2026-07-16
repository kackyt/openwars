declare global {
  interface Window {
    dataLayer?: unknown[];
    gtag?: (...args: unknown[]) => void;
  }
}

/**
 * URL クエリで DebugView 計測が要求されているか（`?debug_mode=1` または `?debug_mode=true`）。
 * gtag.js は URL の debug_mode を自動では読まないため、この判定結果を config に渡して
 * ブラウザ拡張なしで GA4 の DebugView に流せるようにする。
 */
export function isDebugRequested(search: string): boolean {
  const value = new URLSearchParams(search).get("debug_mode");
  return value === "1" || value === "true";
}

/**
 * Google Analytics (gtag.js) を初期化する。計測 ID 未設定なら何もしない。
 * gtag.js は `arguments` オブジェクトが push されることを前提とするため、公式スニペットと
 * 同じ形で実装する。`?debug_mode=1` 付きでアクセスされた場合は DebugView 対象にする。
 */
export function initAnalytics(measurementId: string | undefined): void {
  if (!measurementId) return;

  const script = document.createElement("script");
  script.async = true;
  script.src = `https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(measurementId)}`;
  document.head.appendChild(script);

  window.dataLayer = window.dataLayer ?? [];
  function gtag(): void {
    // biome-ignore lint/complexity/noArguments: gtag.js は Arguments オブジェクトそのものを要求する
    window.dataLayer?.push(arguments);
  }
  window.gtag = gtag as (...args: unknown[]) => void;
  window.gtag("js", new Date());
  // debug_mode を config に渡すと、以降 gtag('event', ...) で送る全イベントが DebugView に載る
  const config = isDebugRequested(window.location.search) ? { debug_mode: true } : {};
  window.gtag("config", measurementId, config);
}

/** GA イベントに添えるパラメータ（GA4 が受け付けるスカラのみ）。 */
export type AnalyticsParams = Record<string, string | number | boolean | undefined>;

/**
 * GA4 へカスタムイベントを送る。gtag 未初期化（計測 ID 未設定・テスト/SSR 環境）では
 * 何もしない no-op。計測の有無を呼び出し側が気にせず済むよう、ここで存在チェックを閉じる。
 */
export function track(event: string, params?: AnalyticsParams): void {
  if (typeof window === "undefined" || typeof window.gtag !== "function") return;
  window.gtag("event", event, params);
}
