const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);

export function requireLoopbackOrigin(value: string, label: string): string {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error(
      `${label} must be a literal HTTP loopback origin without credentials, path, query, or fragment.`,
    );
  }

  if (
    url.protocol !== "http:" ||
    !LOOPBACK_HOSTS.has(url.hostname) ||
    url.username !== "" ||
    url.password !== "" ||
    url.pathname !== "/" ||
    url.search !== "" ||
    url.hash !== ""
  ) {
    throw new Error(
      `${label} must be a literal HTTP loopback origin without credentials, path, query, or fragment.`,
    );
  }

  return url.origin;
}
