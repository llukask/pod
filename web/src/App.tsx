import { Navigate, Route, Router } from "@solidjs/router";
import { Show } from "solid-js";
// Starts the signed-in user's outbox, which resends writes a previous tab left queued.
import "./data/writes";
import { session } from "./session";
import { Shell } from "./ui/Shell";
import { Inbox } from "./ui/Inbox";
import { SignIn } from "./ui/SignIn";

export function App() {
  return (
    <Show when={session()} fallback={<SignIn />}>
      <Router root={Shell}>
        <Route path="/" component={() => <Navigate href="/inbox" />} />
        <Route path="/inbox/:episodeId?" component={Inbox} />
        <Route path="/podcasts" component={() => <div class="page"><h1>Podcasts</h1></div>} />
        <Route path="/settings" component={() => <div class="page"><h1>Settings</h1></div>} />
        <Route path="*" component={() => <Navigate href="/inbox" />} />
      </Router>
    </Show>
  );
}
