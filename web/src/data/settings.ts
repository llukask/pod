import { createResource, createRoot } from "solid-js";
import { playerSettings } from "../player/settings";
import { api, session } from "../session";
import { write } from "./writes";

// The synced settings (`GET /v1/settings`, spec 2.4), every known key at its
// default plus whatever else is stored. Changing one updates this copy at once
// and queues a `setting.set`.
const [stored, { mutate }] = createRoot(() =>
  createResource(
    () => session()?.token,
    async () => (await api<{ settings: Record<string, unknown> }>("GET", "/v1/settings")).settings,
  ),
);

export function settingValues(): Record<string, unknown> {
  return stored() ?? {};
}

export function currentPlayerSettings() {
  return playerSettings(settingValues());
}

export function setSetting(key: string, value: unknown) {
  mutate({ ...settingValues(), [key]: value });
  write({ type: "setting.set", key, value });
}
