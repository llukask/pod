import { expect, test } from "vitest";
import { SseParser } from "./sse";

test("each data line followed by a blank line is one event", () => {
  const parser = new SseParser();
  expect(parser.push('data: {"type":"state"}\n\ndata: {"type":"release"}\n\n')).toEqual(['{"type":"state"}', '{"type":"release"}']);
});

test("an event waits for its blank line", () => {
  const parser = new SseParser();
  expect(parser.push('data: {"type":"state"}\n')).toEqual([]);
  expect(parser.push("\n")).toEqual(['{"type":"state"}']);
});

test("several data lines make one payload, joined by line breaks", () => {
  const parser = new SseParser();
  expect(parser.push("data: first\ndata: second\ndata\n\n")).toEqual(["first\nsecond\n"]);
});

test("comments and the server's pings are skipped", () => {
  const parser = new SseParser();
  expect(parser.push(": ping\n\n: ping\n\ndata: x\n\n")).toEqual(["x"]);
});

test("fields other than data are ignored, and only one leading space is dropped", () => {
  const parser = new SseParser();
  expect(parser.push("event: state\nid: 7\nretry: 1000\ndata:  two spaces\n\n")).toEqual([" two spaces"]);
  expect(parser.push("data:none\n\n")).toEqual(["none"]);
});

test("an event cut anywhere across chunks comes out whole", () => {
  const stream = 'data: {"type":"state","devices":[]}\n\n: ping\n\ndata: {"type":"load"}\n\n';
  for (let cut = 1; cut < stream.length; cut++) {
    const parser = new SseParser();
    const events = [...parser.push(stream.slice(0, cut)), ...parser.push(stream.slice(cut))];
    expect(events, `cut at ${cut}`).toEqual(['{"type":"state","devices":[]}', '{"type":"load"}']);
  }
});

test("CRLF and CR line breaks work, a CRLF split between chunks included", () => {
  expect(new SseParser().push("data: a\r\n\r\ndata: b\r\rdata: c\n\n")).toEqual(["a", "b", "c"]);
  const parser = new SseParser();
  expect(parser.push("data: a\r")).toEqual([]);
  expect(parser.push("\n\r")).toEqual(["a"]);
  expect(parser.push("\ndata: b\n\n")).toEqual(["b"]);
});

test("a blank line with no data before it is no event", () => {
  expect(new SseParser().push("\n\n\n")).toEqual([]);
});
