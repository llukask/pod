import { createResource, createRoot } from "solid-js";
import type { Page, Podcast } from "../api/types";
import { api, session } from "../session";

// The subscribed shows (`GET /v1/podcasts`, spec 4.5), shared by the sidebar
// and the podcast pages. It loads once per session and again after a
// subscribe or unsubscribe.
const [podcasts, { refetch: refetchPodcasts, mutate: setPodcasts }] = createRoot(() =>
  createResource(
    () => session()?.token,
    async () => (await api<Page<Podcast>>("GET", "/v1/podcasts")).items,
  ),
);

export { podcasts, refetchPodcasts, setPodcasts };
