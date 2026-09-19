import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/app.css";

const el = document.getElementById("root");
if (!el) throw new Error("#root is missing from the document");

// StrictMode stays on in development on purpose: it double-invokes effects,
// which is exactly the pressure that exposes a poller or an auth prompt that
// is not idempotent across mount → unmount → mount.
createRoot(el).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
