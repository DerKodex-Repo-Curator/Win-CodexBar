import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { buildProofScenario } from "./fixtures/scenarios";
import { PROOF_WINDOWS, windowLabel, type ProofParams } from "./params";
import { createProofRouter, type ProofRouter } from "./router";

/**
 * Replace the Tauri IPC with fixture data. Must run before App mounts:
 * `getCurrentWebviewWindow()` reads the mocked window metadata.
 */
export function installProofMocks(params: ProofParams, now: Date = new Date()): ProofRouter {
  const scenario = buildProofScenario(params.fixture, now);
  if (params.theme) {
    scenario.settings = { ...scenario.settings, theme: params.theme };
  }
  const router = createProofRouter(scenario, { window: params.window, tab: params.tab, now });
  const current = windowLabel(params.window);
  const others = PROOF_WINDOWS.map(windowLabel).filter((label) => label !== current);
  mockWindows(current, ...others);
  mockIPC(router.handle, { shouldMockEvents: true });
  return router;
}
