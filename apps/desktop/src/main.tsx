import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "@/styles/app.css";

import { App } from "@/host/App";
import { connect } from "@/lib/core";

const container = document.getElementById("root");
if (!container) throw new Error("index.html has no #root element.");
const root = createRoot(container);

connect().then(
  (client) => {
    root.render(
      <StrictMode>
        <App client={client} />
      </StrictMode>,
    );
  },
  (thrown: unknown) => {
    root.render(
      <div role="alert" className="grid h-full place-items-center p-8">
        <p className="max-w-[68ch] text-app-text">
          {thrown instanceof Error ? thrown.message : String(thrown)}
        </p>
      </div>,
    );
  },
);
