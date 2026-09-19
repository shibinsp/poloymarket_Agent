import type { ReactNode } from "react";

/**
 * A plain anchor. No `preventDefault`, so middle-click, ctrl-click, "copy link
 * address" and the browser's own history all work without being reimplemented.
 * Active state is `aria-current`, which is both the accessible signal and the
 * CSS hook.
 */
export function Link({
  to,
  active,
  className,
  children,
}: {
  to: string;
  active?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <a
      href={"#" + to}
      className={className}
      aria-current={active ? "page" : undefined}
    >
      {children}
    </a>
  );
}
