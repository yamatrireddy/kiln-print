// Starts a real kiln-agent (debug build) with simulated printers for SDK tests.
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repo = resolve(import.meta.dirname, "..", "..", "..");
export const agentExe = join(repo, "target", "debug", process.platform === "win32" ? "kiln-agent.exe" : "kiln-agent");
export const agentAvailable = existsSync(agentExe);

export async function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const server = net.createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as net.AddressInfo;
      server.close(() => ok(port));
    });
  });
}

export class TestAgent {
  readonly dir = mkdtempSync(join(tmpdir(), "kiln-sdk-"));
  readonly token = `kiln_sdk_test_${Math.random().toString(36).slice(2)}_${Date.now()}`;
  #child: ChildProcess | null = null;
  port = 0;

  get url(): string {
    return `ws://127.0.0.1:${this.port}/v1/ws`;
  }

  async start(port?: number): Promise<void> {
    this.port = port ?? (this.port || (await freePort()));
    const tokenFile = join(this.dir, "admin.token");
    writeFileSync(tokenFile, this.token);
    const config = join(this.dir, "agent.toml");
    writeFileSync(
      config,
      [
        "[server]",
        `bind = "127.0.0.1:${this.port}"`,
        "[security]",
        `admin_token_file = '${tokenFile}'`,
        "[security.rate_limit]",
        "requests_per_second = 1000.0",
        "burst = 2000",
        "[jobs]",
        "monitor_interval_ms = 50",
        "monitor_max_interval_ms = 100",
        "[providers]",
        "windows = false",
        "mock = true",
        "[storage]",
        `data_dir = '${this.dir}'`,
        "[logging]",
        "file = false",
        "console = false",
      ].join("\n"),
    );
    this.#child = spawn(agentExe, ["--config", config, "run"], { stdio: "ignore" });
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      try {
        const res = await fetch(`http://127.0.0.1:${this.port}/v1/health`);
        if (res.ok) return;
      } catch {
        // not up yet
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    throw new Error("kiln-agent did not start");
  }

  async kill(): Promise<void> {
    const child = this.#child;
    this.#child = null;
    if (!child || child.exitCode !== null) return;
    await new Promise<void>((resolve) => {
      child.once("exit", () => resolve());
      child.kill();
    });
  }

  async stop(): Promise<void> {
    await this.kill();
    rmSync(this.dir, { recursive: true, force: true });
  }
}
