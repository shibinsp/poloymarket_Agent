import { IndiaOrbLedger, UsOrbLedger } from "../components/live/StrategyLedgers";

/** The two strategies the agent trades, each from its own ledger. */
export function Trades() {
  return (
    <div className="page">
      <IndiaOrbLedger />
      <UsOrbLedger />
    </div>
  );
}
