import { Link } from "../router/Link";
import { ROUTES } from "../router/routes";

/**
 * An unknown hash says so rather than redirecting. A silent bounce to the
 * overview hides a broken bookmark or a typo in a link, and the operator never
 * learns the URL was wrong.
 */
export function NotFound({ attempted }: { attempted: string | null }) {
  return (
    <div className="page">
      <div className="card">
        <div className="card__body">
          <h1>No such page</h1>
          <p className="muted">
            Nothing is routed at <code>{attempted ?? "that address"}</code>.
          </p>
          <ul>
            {ROUTES.map((r) => (
              <li key={r.path}>
                <Link to={r.path}>{r.label}</Link>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
