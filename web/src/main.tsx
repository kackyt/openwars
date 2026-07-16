import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App.tsx";
import { initAnalytics } from "./utils/analytics.ts";
import { gaMeasurementId } from "./utils/config.ts";

initAnalytics(gaMeasurementId());

const container = document.getElementById("root");
if (container) {
  const root = createRoot(container);
  root.render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
