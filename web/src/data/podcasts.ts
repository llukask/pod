import { createResource, createRoot } from "solid-js";
import type { Page, Podcast } from "../api/types";
import { api, session } from "../session";

// The subscribed shows (`GET /v1/podcasts`, spec 4.5), shared by the sidebar
// and the podcast pages. It loads once per session and again after a
// subscribe or unsubscribe.
//
// A show the server hasn't fetched yet comes back pending with no title or
// artwork. When this tab knows them already, from the directory result it
// subscribed from, it keeps them until the server has its own.
const [podcasts, { refetch: refetchPodcasts, mutate: setPodcasts }] = createRoot(() =>
  createResource(
    () => session()?.token,
    async (_token, { value: previous }: { value: Podcast[] | undefined }) => {
      const known = new Map((previous ?? []).map((p) => [p.id, p]));
      return (await api<Page<Podcast>>("GET", "/v1/podcasts")).items.map((podcast) => {
        const before = known.get(podcast.id);
        if (podcast.status !== "pending" || !before) return podcast;
        return { ...podcast, title: podcast.title ?? before.title, artwork_url: podcast.artwork_url ?? before.artwork_url };
      });
    },
  ),
);

export { podcasts, refetchPodcasts, setPodcasts };
