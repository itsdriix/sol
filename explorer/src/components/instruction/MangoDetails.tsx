import { SignatureResult, TransactionInstruction } from "@solana/web3.js";
import { useCluster } from "providers/cluster";
import { reportError } from "utils/sentry";
import { InstructionCard } from "./InstructionCard";
import { AddOracleDetailsCard } from "./mango/AddOracleDetailsCard";
import { AddSpotMarketDetailsCard } from "./mango/AddSpotMarketDetailsCard";
import { CancelPerpOrderDetailsCard } from "./mango/CancelPerpOrderDetailsCard";
import { CancelSpotOrderDetailsCard } from "./mango/CancelSpotOrderDetailsCard";
import { ChangePerpMarketParamsDetailsCard } from "./mango/ChangePerpMarketParamsDetailsCard";
import { GenericConsumeEventsDetailsCard } from "./mango/GenericConsumeEventsDetailsCard";
import { GenericMngoAccountDetailsCard } from "./mango/GenericMngoAccountDetailsCard";
import { GenericPerpMngoDetailsCard } from "./mango/GenericPerpMngoDetailsCard";
import { GenericSpotMngoDetailsCard } from "./mango/GenericSpotMngoDetailsCard";
import { PlacePerpOrderDetailsCard } from "./mango/PlacePerpOrderDetailsCard";
import { PlaceSpotOrderDetailsCard } from "./mango/PlaceSpotOrderDetailsCard";
import {
  decodeAddSpotMarket,
  decodeCancelPerpOrder,
  decodeCancelSpotOrder,
  decodeChangePerpMarketParams,
  decodePlacePerpOrder,
  decodePlaceSpotOrder,
  parseMangoInstructionTitle,
} from "./mango/types";

export function MangoDetailsCard(props: {
  ix: TransactionInstruction;
  index: number;
  result: SignatureResult;
  signature: string;
  innerCards?: JSX.Element[];
  childIndex?: number;
}) {
  const { ix, index, result, signature, innerCards, childIndex } = props;

  const { url } = useCluster();

  let title;
  try {
    title = parseMangoInstructionTitle(ix);

    // todo
    // AddPerpMarket
    switch (title) {
      case "InitMangoAccount":
        return (
          <GenericMngoAccountDetailsCard
            mangoAccountKeyLocation={1}
            title={"InitMangoAccount"}
            {...props}
          />
        );
      case "Deposit":
        return (
          <GenericMngoAccountDetailsCard
            mangoAccountKeyLocation={1}
            title={"Deposit"}
            {...props}
          />
        );
      case "Withdraw":
        return (
          <GenericMngoAccountDetailsCard
            mangoAccountKeyLocation={1}
            title={"Withdraw"}
            {...props}
          />
        );
      case "InitSpotOpenOrders":
        return (
          <GenericMngoAccountDetailsCard
            mangoAccountKeyLocation={1}
            title={"InitSpotOpenOrders"}
            {...props}
          />
        );
      case "PlaceSpotOrder":
        return (
          <PlaceSpotOrderDetailsCard
            info={decodePlaceSpotOrder(ix)}
            {...props}
          />
        );
      case "CancelSpotOrder":
        return (
          <CancelSpotOrderDetailsCard
            info={decodeCancelSpotOrder(ix)}
            {...props}
          />
        );
      case "PlacePerpOrder":
        return (
          <PlacePerpOrderDetailsCard
            info={decodePlacePerpOrder(ix)}
            {...props}
          />
        );
      case "ConsumeEvents":
        return <GenericConsumeEventsDetailsCard {...props} />;
      case "CancelPerpOrder":
        return (
          <CancelPerpOrderDetailsCard
            info={decodeCancelPerpOrder(ix)}
            {...props}
          />
        );
      case "SettleFunds":
        return (
          <GenericSpotMngoDetailsCard
            accountKeyLocation={2}
            spotMarketkeyLocation={5}
            title={"SettleFunds"}
            {...props}
          />
        );
      case "RedeemMngo":
        return (
          <GenericPerpMngoDetailsCard
            mangoAccountKeyLocation={3}
            perpMarketKeyLocation={4}
            title={"RedeemMngo"}
            {...props}
          />
        );
      case "ChangePerpMarketParams":
        return (
          <ChangePerpMarketParamsDetailsCard
            info={decodeChangePerpMarketParams(ix)}
            {...props}
          />
        );
      case "AddOracle":
        return <AddOracleDetailsCard {...props} />;
      case "AddSpotMarket":
        return (
          <AddSpotMarketDetailsCard info={decodeAddSpotMarket(ix)} {...props} />
        );
    }
  } catch (error) {
    reportError(error, {
      url: url,
      signature: signature,
    });
  }

  return (
    <InstructionCard
      ix={ix}
      index={index}
      result={result}
      title={`Mango: ${title || "Unknown"}`}
      innerCards={innerCards}
      childIndex={childIndex}
      defaultRaw
    />
  );
}
