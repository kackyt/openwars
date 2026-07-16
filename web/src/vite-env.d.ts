declare module "*.wasm?url" { const src: string; export default src; }

interface ImportMetaEnv {
  /** Google Analytics 計測 ID（例: G-XXXXXXXXXX）。未設定なら計測しない。 */
  readonly VITE_GA_MEASUREMENT_ID?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
