import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyDensityPreference, applyStylePreference, applyThemePreference, getStoredDensityPreference, getStoredStylePreference, getStoredThemePreference } from "./themes";
import "./themes/index.css";
import "./styles.css";

applyThemePreference(getStoredThemePreference());
applyStylePreference(getStoredStylePreference());
applyDensityPreference(getStoredDensityPreference());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
