import type { components } from "./schema";

// Names for the generated wire types (spec 4) the client uses.
type Schemas = components["schemas"];

export type TokenResponse = Schemas["TokenResponse"];
export type Me = Schemas["MeResponse"];
export type Podcast = Schemas["PodcastItem"];
export type Episode = Schemas["EpisodeItem"];
export type EpisodeState = Schemas["EpisodeState"];
export type InboxItem = Schemas["InboxItem"];
export type Mutation = Schemas["Mutation"];
export type MutationResult = Schemas["MutationResult"];
export type SyncResponse = Schemas["SyncResponse"];
export type DirectoryResult = Schemas["DirectoryResult"];

/** A keyset page (spec 4.5). `next_cursor` is absent or null on the last page. */
export type Page<T> = { items: T[]; next_cursor?: string | null };
