import { Navigate, Route, Router } from "@solidjs/router";
import { Show } from "solid-js";
// Starts the signed-in user's outbox, which resends writes a previous tab left queued.
import "./data/writes";
// Reports subscribes the server rejects, a queued one from an earlier tab included.
import "./data/subscriptions";
import { session } from "./session";
import { Shell } from "./ui/Shell";
import { Inbox } from "./ui/Inbox";
import { PodcastRoute } from "./ui/Podcast";
import { Podcasts } from "./ui/Podcasts";
import { Search } from "./ui/Search";
import { SignIn } from "./ui/SignIn";

export function App() {
  return (
    <Show when={session()} fallback={<SignIn />}>
      <Router root={Shell}>
        <Route path="/" component={() => <Navigate href="/inbox" />} />
        <Route path="/inbox/:episodeId?" component={Inbox} />
        <Route path="/podcasts" component={Podcasts} />
        <Route path="/podcasts/:podcastId/:episodeId?" component={PodcastRoute} />
        <Route path="/search" component={Search} />
        <Route path="/settings" component={() => <div class="page"><h1>Settings</h1></div>} />
        <Route path="*" component={() => <Navigate href="/inbox" />} />
      </Router>
    </Show>
  );
}
