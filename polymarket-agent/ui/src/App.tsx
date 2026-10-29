import { Component, useEffect, useSyncExternalStore, type ReactNode } from "react";
import { AppShell } from "./components/shell/AppShell";
import { useHashRoute } from "./router/useHashRoute";
import { Overview } from "./pages/Overview";
import { Trades } from "./pages/Trades";
import { Orders } from "./pages/Orders";
import { Risk } from "./pages/Risk";
import { Cycles } from "./pages/Cycles";
import { LearnFromMistakes } from "./pages/LearnFromMistakes";
import { Costs } from "./pages/Costs";
import { IndiaTest } from "./pages/IndiaTest";
import { UsTest } from "./pages/UsTest";
import { Logs } from "./pages/Logs";
import { Health } from "./pages/Health";
import { Settings } from "./pages/Settings";
import { NotFound } from "./pages/NotFound";
import { applyTheme, settings } from "./data/settings";

interface ErrorBoundaryProps {
  children: ReactNode;
}
interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false, error: null };
  }
  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }
  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("ErrorBoundary caught:", error, errorInfo);
  }
  render() {
    if (this.state.hasError) {
      return (
        <section className="error-boundary" role="alert">
          <h2>Unable to render this page</h2>
          <p>The dashboard encountered an unexpected error.</p>
          <pre>
            {this.state.error?.message || String(this.state.error)}
          </pre>
          <button
            className="btn btn--primary"
            onClick={() => window.location.reload()}
          >
            Reload dashboard
          </button>
        </section>
      );
    }
    return this.props.children;
  }
}

export function App() {
  const { path, route, attempted } = useHashRoute();
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);

  useEffect(() => {
    applyTheme(cfg.theme);
  }, [cfg.theme]);

  return (
    <AppShell activePath={path}>
      <ErrorBoundary>
        {!route ? (
          <NotFound attempted={attempted} />
        ) : path === "/trades" ? (
          <Trades />
        ) : path === "/orders" ? (
          <Orders />
        ) : path === "/risk" ? (
          <Risk />
        ) : path === "/cycles" ? (
          <Cycles />
        ) : path === "/learn" ? (
          <LearnFromMistakes />
        ) : path === "/costs" ? (
          <Costs />
        ) : path === "/india" ? (
          <IndiaTest />
        ) : path === "/us" ? (
          <UsTest />
        ) : path === "/logs" ? (
          <Logs />
        ) : path === "/health" ? (
          <Health />
        ) : path === "/settings" ? (
          <Settings />
        ) : (
          <Overview />
        )}
      </ErrorBoundary>
    </AppShell>
  );
}
