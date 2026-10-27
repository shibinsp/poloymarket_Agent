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
        <div style={{ padding: "40px", maxWidth: "800px", margin: "40px auto", background: "#1e1e24", borderRadius: "10px", border: "1px solid #ef4444", color: "#fff" }}>
          <h2 style={{ color: "#ef4444", margin: "0 0 16px" }}>Page Render Error</h2>
          <pre style={{ background: "#111", padding: "16px", borderRadius: "6px", overflowX: "auto", color: "#f87171" }}>
            {this.state.error?.message || String(this.state.error)}
          </pre>
          <button
            onClick={() => window.location.reload()}
            style={{ marginTop: "16px", padding: "8px 16px", background: "#3b82f6", color: "#fff", border: "none", borderRadius: "6px", cursor: "pointer", fontWeight: 700 }}
          >
            Reload Page
          </button>
        </div>
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
