import { useEffect, useSyncExternalStore } from "react";
import { AppShell } from "./components/shell/AppShell";
import { useHashRoute } from "./router/useHashRoute";
import { Overview } from "./pages/Overview";
import { Trades } from "./pages/Trades";
import { Orders } from "./pages/Orders";
import { Risk } from "./pages/Risk";
import { Cycles } from "./pages/Cycles";
import { Costs } from "./pages/Costs";
import { Health } from "./pages/Health";
import { Settings } from "./pages/Settings";
import { NotFound } from "./pages/NotFound";
import { applyTheme, settings } from "./data/settings";

export function App() {
  const { path, route, attempted } = useHashRoute();
  const cfg = useSyncExternalStore(settings.subscribe, settings.getSnapshot, settings.getSnapshot);

  useEffect(() => {
    applyTheme(cfg.theme);
  }, [cfg.theme]);

  return (
    <AppShell activePath={path}>
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
      ) : path === "/costs" ? (
        <Costs />
      ) : path === "/health" ? (
        <Health />
      ) : path === "/settings" ? (
        <Settings />
      ) : (
        <Overview />
      )}
    </AppShell>
  );
}
