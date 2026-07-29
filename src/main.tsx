import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { DatabaseUpgrade } from "./components/DatabaseUpgrade";
import { UpdateProvider } from "./contexts/UpdateContext";
import "./index.css";
// Load internationalization configuration.
import i18n from "./i18n";
import { QueryClientProvider } from "@tanstack/react-query";
import { ThemeProvider } from "@/components/theme-provider";
import { queryClient } from "@/lib/query";
import { Toaster } from "@/components/ui/sonner";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { exit } from "@tauri-apps/plugin-process";

// Add a platform-specific body class for platform-specific styling.
try {
  const ua = navigator.userAgent || "";
  const plat = (navigator.platform || "").toLowerCase();
  const isMac = /mac/i.test(ua) || plat.includes("mac");
  if (isMac) {
    document.body.classList.add("is-mac");
  }
} catch {
  // Ignore platform detection failures.
}

// Configuration-load error payload.
interface ConfigLoadErrorPayload {
  path?: string;
  error?: string;
  /** "db_version_too_new" renders the in-app upgrade recovery screen. */
  kind?: string;
}

/**
 * Handles configuration-load failures by displaying an error and forcing the
 * application to exit. A damaged configuration prevents normal operation, so
 * the dialog intentionally provides no cancel option.
 */
async function handleConfigLoadError(
  payload: ConfigLoadErrorPayload | null,
): Promise<void> {
  const path = payload?.path ?? "~/.nexus-composer/config.json";
  const detail = payload?.error ?? "Unknown error";

  await message(
    i18n.t("errors.configLoadFailedMessage", {
      path,
      detail,
      defaultValue:
        "Unable to read configuration file:\n{{path}}\n\nError details:\n{{detail}}\n\nPlease check if the JSON is valid, or restore from a backup file (e.g., config.json.bak) in the same directory.\n\nThe app will exit so you can fix this.",
    }),
    {
      title: i18n.t("errors.configLoadFailedTitle", {
        defaultValue: "Configuration Load Failed",
      }),
      kind: "error",
    },
  );

  await exit(1);
}

// Listen for backend configuration-load errors. Notify the user and force an
// exit without modifying configuration files.
try {
  void listen("configLoadError", async (evt) => {
    await handleConfigLoadError(evt.payload as ConfigLoadErrorPayload | null);
  });
} catch (e) {
  // Ignore subscription failures, such as in a non-Tauri environment.
  console.error("Failed to subscribe to the configLoadError event", e);
}

async function bootstrap() {
  // Query backend initialization errors early to avoid an event race.
  try {
    const initError = (await invoke(
      "get_init_error",
    )) as ConfigLoadErrorPayload | null;
    if (initError && initError.kind === "db_version_too_new") {
      // If the database is newer than the app, render the in-app upgrade
      // recovery screen instead of the normal application.
      ReactDOM.createRoot(document.getElementById("root")!).render(
        <React.StrictMode>
          <ThemeProvider
            defaultTheme="system"
            storageKey="nexus-composer-theme"
          >
            <DatabaseUpgrade payload={initError} />
            <Toaster />
          </ThemeProvider>
        </React.StrictMode>,
      );
      return;
    }
    if (initError && (initError.path || initError.error)) {
      await handleConfigLoadError(initError);
      // This is unreachable because exit(1) terminates the process.
      return;
    }
  } catch (e) {
    // Ignore retrieval failures and continue rendering.
    console.error("Failed to retrieve the initialization error", e);
  }

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <ThemeProvider defaultTheme="system" storageKey="nexus-composer-theme">
          <UpdateProvider>
            <App />
            <Toaster />
          </UpdateProvider>
        </ThemeProvider>
      </QueryClientProvider>
    </React.StrictMode>,
  );
}

void bootstrap();
