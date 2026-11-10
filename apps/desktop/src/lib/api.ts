import { invoke } from "@tauri-apps/api/core";

import type { ServerStatus } from "../types";
import { isDesktopApp } from "./platform";
import { requestServer } from "./transport";

/** Host lifecycle. Only the desktop owns a process, so start/stop/restart
 * stay Tauri commands; the browser only ever reports what serves the page. */
export const api = {
  async getServerStatus(): Promise<ServerStatus> {
    if (isDesktopApp()) {
      return invoke("get_server_status") as Promise<ServerStatus>;
    }
    // The gateway is answering this page; prove it and report what the
    // browser is actually talking to.
    await requestServer({ method: "GET", path: "/v1/admin/status" });
    const port = window.location.port
      ? Number(window.location.port)
      : window.location.protocol === "https:"
        ? 443
        : 80;
    return { state: "running", host: window.location.hostname, port };
  },

  startServer(): Promise<ServerStatus> {
    return invoke("start_server");
  },

  stopServer(): Promise<ServerStatus> {
    return invoke("stop_server");
  },

  restartServer(): Promise<ServerStatus> {
    return invoke("restart_server");
  },
};
