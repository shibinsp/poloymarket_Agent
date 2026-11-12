import { CryptoLedger, UsOptionsLedger, UsOrbLedger } from "../components/live/StrategyLedgers";

/** The U.S. opening-range breakout on the Alpaca paper account, its options book and the crypto books. */
export function UsTest() {
  return (
    <div className="page">
      <UsOrbLedger />
      <UsOptionsLedger />
      <CryptoLedger />
    </div>
  );
}
