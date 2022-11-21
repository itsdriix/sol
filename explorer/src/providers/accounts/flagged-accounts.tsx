import React from "react";
import { fetch } from "cross-fetch";
import { Link } from "react-router-dom";

const FLAGGED_REGISTRY =
  "https://solana-labs.github.io/solana-flagged-accounts/flagged.txt";

type FlaggedMap = Record<string, IncidentDescription>;
type ProviderProps = { children: React.ReactNode };

type IncidentId = "ftx-hack-november-2022" | "known-scam";
type IncidentDescription = React.ReactElement;

const FLAGGED_ACCOUNTS: Record<string, IncidentId> = {
  GACpXND1SSfTSQMmqGuFvGwXB3jGEYBDRGNzmLfTYwSP: "known-scam",
  "9tAViia54YAaL9gv92hBu8K4QGRBKbytCQ9TYsJ6F6or": "known-scam",
  // Serum Swap
  "22Y43yTVxuUkoRKdm9thyRhQ3SdgQS7c7kB6UNCiaczD": "ftx-hack-november-2022",
  // Serum Dex V3
  "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin": "ftx-hack-november-2022",
  // Serum Dex V2
  EUqojwWA2rd19FZrzeBncJsm38Jm1hEhE3zsmX3bRc2o: "ftx-hack-november-2022",
  // Serum Dex V1
  BJ3jrUzddfuSrZHXSCxMUUQsjKEyLmuuyZebkcaFp2fg: "ftx-hack-november-2022",
};
const INCIDENTS: Record<IncidentId, IncidentDescription> = {
  "known-scam": (
    <>
      <div className="alert alert-danger alert-scam" role="alert">
        Warning! This account has been flagged by the community as a scam
        account. Please be cautious sending SOL to this account.
      </div>
    </>
  ),
  "ftx-hack-november-2022": (
    <>
      <div className="alert alert-danger alert-scam" role="alert">
        Warning! This program's upgrade key may have been compromised by the FTX
        hack. Please migrate to the community fork:{" "}
        <Link
          className="text-white"
          style={{ textDecoration: "underline" }}
          to="https://github.com/openbook-dex/program"
        >
          https://github.com/openbook-dex/program
        </Link>
      </div>
    </>
  ),
} as const;

const FlaggedContext = React.createContext<FlaggedMap>({});

export function FlaggedAccountsProvider({ children }: ProviderProps) {
  const [flaggedAccounts, setFlaggedAccounts] = React.useState<FlaggedMap>({});

  React.useEffect(() => {
    let flaggedMap: FlaggedMap = {};
    for (const [account, incidentId] of Object.entries(FLAGGED_ACCOUNTS)) {
      flaggedMap[account] = INCIDENTS[incidentId];
    }
    setFlaggedAccounts(flaggedMap);
  }, []);

  return (
    <FlaggedContext.Provider value={flaggedAccounts}>
      {children}
    </FlaggedContext.Provider>
  );
}

export function useFlaggedAccounts() {
  const flaggedAccounts = React.useContext(FlaggedContext);
  if (!flaggedAccounts) {
    throw new Error(
      `useFlaggedAccounts must be used within a AccountsProvider`
    );
  }

  return { flaggedAccounts };
}
