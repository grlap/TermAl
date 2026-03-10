import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyThemePreference, getStoredThemePreference } from "./themes";
import "./themes/index.css";
import "./styles.css";

applyThemePreference(getStoredThemePreference());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
