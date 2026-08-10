import { useSyncExternalStore, type MouseEvent, type ReactNode } from "react";

const NAVIGATION_EVENT = "migration-stream:navigate";

function locationSnapshot(): string {
  return `${window.location.pathname}${window.location.search}${window.location.hash}`;
}

function subscribe(listener: () => void): () => void {
  window.addEventListener("popstate", listener);
  window.addEventListener("hashchange", listener);
  window.addEventListener(NAVIGATION_EVENT, listener);
  return () => {
    window.removeEventListener("popstate", listener);
    window.removeEventListener("hashchange", listener);
    window.removeEventListener(NAVIGATION_EVENT, listener);
  };
}

export function useLocation(): URL {
  const location = useSyncExternalStore(
    subscribe,
    locationSnapshot,
    locationSnapshot,
  );
  return new URL(location, window.location.origin);
}

export function navigate(href: string, replace = false): void {
  const target = new URL(href, window.location.origin);
  if (target.origin !== window.location.origin) return;
  const next = `${target.pathname}${target.search}${target.hash}`;
  if (replace) window.history.replaceState(null, "", next);
  else window.history.pushState(null, "", next);
  window.dispatchEvent(new Event(NAVIGATION_EVENT));
  if (target.hash === "") {
    window.scrollTo?.({ top: 0, behavior: "auto" });
    return;
  }
  window.requestAnimationFrame?.(() => {
    let identifier: string;
    try {
      identifier = decodeURIComponent(target.hash.slice(1));
    } catch {
      return;
    }
    document.getElementById(identifier)?.scrollIntoView?.({
      block: "start",
      behavior: "auto",
    });
  });
}

export function AppLink({
  href,
  children,
  className,
  current,
}: {
  href: string;
  children: ReactNode;
  className?: string;
  current?: boolean;
}) {
  function follow(event: MouseEvent<HTMLAnchorElement>): void {
    if (
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    ) {
      return;
    }
    event.preventDefault();
    navigate(href);
  }

  return (
    <a
      href={href}
      onClick={follow}
      className={className}
      aria-current={current ? "page" : undefined}
    >
      {children}
    </a>
  );
}
