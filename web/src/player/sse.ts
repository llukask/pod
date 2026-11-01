// ==============================================================================
// Server-sent events, parsed by hand
// ==============================================================================
//
// `EventSource` can't send an `Authorization` header, so the Connect stream
// (spec 14.2) is read with `fetch` and fed through this parser. It follows the
// parts of the WHATWG event-stream format the server uses: `data:` lines,
// several of which make one multi-line payload, comment lines (`: ping`), and
// a blank line that ends an event. Chunks arrive cut anywhere, a line or a
// `\r\n` included, so the parser keeps what it hasn't seen the end of.

export class SseParser {
  /** Text after the last line break, waiting for the rest of its line. */
  private partial = "";
  /** The `data:` lines of the event being read. */
  private data: string[] = [];
  /** The last chunk ended in `\r`, so a `\n` that starts the next one belongs to it. */
  private afterCr = false;

  /** Feeds a decoded chunk and returns the payload of each event it completed, in order. */
  push(chunk: string): string[] {
    let text = chunk;
    if (this.afterCr && text.startsWith("\n")) text = text.slice(1);
    this.afterCr = text.endsWith("\r");

    const lines = (this.partial + text).split(/\r\n|\r|\n/);
    // The last piece has no line break after it yet. A chunk that ended in
    // `\r` already ended its line, which `split` shows as an empty last piece.
    this.partial = lines.pop() ?? "";

    const events: string[] = [];
    for (const line of lines) {
      if (line === "") {
        if (this.data.length > 0) events.push(this.data.join("\n"));
        this.data = [];
      } else if (line.startsWith(":")) {
        // A comment, such as the server's keep-alive ping.
      } else {
        const colon = line.indexOf(":");
        const field = colon === -1 ? line : line.slice(0, colon);
        let value = colon === -1 ? "" : line.slice(colon + 1);
        if (value.startsWith(" ")) value = value.slice(1);
        // `event`, `id` and `retry` mean nothing to this stream: the type is in the JSON.
        if (field === "data") this.data.push(value);
      }
    }
    return events;
  }
}
