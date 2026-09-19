import type { ReactNode } from "react";
import { cls } from "../../lib/cls";

export function Card({
  title,
  actions,
  footer,
  dimmed,
  children,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  footer?: ReactNode;
  dimmed?: boolean;
  children: ReactNode;
}) {
  return (
    <section className="card">
      {(title || actions) && (
        <div className="card__head">
          {title && <h2>{title}</h2>}
          {actions && <div className="spacer">{actions}</div>}
        </div>
      )}
      <div className={cls("card__body", dimmed && "is-dimmed")}>{children}</div>
      {footer && <div className="card__foot">{footer}</div>}
    </section>
  );
}
