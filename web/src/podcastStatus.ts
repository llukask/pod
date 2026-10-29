import type { components } from "./api/schema";

// The banner of a failing or gone feed (D7 annotation 2), worded as on the
// desktop (`podcast::status_notice`) and with the protocol's error sentences
// (`FeedError::message`).

type FeedStatus = components["schemas"]["FeedStatus"];
type FeedError = components["schemas"]["FeedError"];

/** One short sentence without a trailing period, never with the URL in it (spec 3.2). */
export function feedErrorMessage(error: FeedError): string {
  switch (error.code) {
    case "http":
      return `The feed returned HTTP ${error.status}`;
    case "not_a_feed":
      return "The URL doesn't point to a podcast feed";
    case "invalid_url":
      return "The feed URL is invalid";
    case "port_not_allowed":
      return "The feed's port is not allowed";
    case "address_not_allowed":
      return "The feed's address is not allowed";
    case "host_not_found":
      return "The feed's host can't be found";
    case "too_many_redirects":
      return "The feed redirects too many times";
    case "invalid_redirect":
      return "The feed redirects somewhere invalid";
    case "too_large":
      return "The feed is too large";
    case "timeout":
      return "The feed timed out";
    case "connection_failed":
      return "The feed's server can't be reached";
    default:
      // The code set is open; a newer server may send one this client predates.
      return "The feed can't be fetched";
  }
}

export function statusNotice(status: FeedStatus, now: Date = new Date()): string | null {
  switch (status.status) {
    case "failing":
      return `${feedErrorMessage(status.error)}. Pod has kept trying for ${failingFor(status.failing_since, now)} and will keep checking for new episodes.`;
    case "gone":
      return "The publisher removed this feed, and Pod has stopped checking for new episodes.";
    default:
      return null;
  }
}

function failingFor(since: string, now: Date): string {
  const hours = Math.floor((now.getTime() - Date.parse(since)) / 3_600_000);
  const days = Math.floor(hours / 24);
  if (days >= 1) return days === 1 ? "1 day" : `${days} days`;
  if (hours >= 1) return hours === 1 ? "1 hour" : `${hours} hours`;
  return "a few minutes";
}
