import { render } from "solid-js/web";
import { App } from "./App";
// Bundled, not fetched: the CSP allows fonts from this origin only.
import "@fontsource-variable/radio-canada/wdth.css";
import "@fontsource-variable/radio-canada-big/wght.css";
import "./styles.css";

render(() => <App />, document.getElementById("root")!);
