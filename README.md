# Distribute Solana tokens

A user may want to make payments to multiple accounts over multiple iterations.
The user will have a spreadsheet listing public keys and token amounts, and
some process for transferring tokens to them, and ensuring that no more than the
expected amount are sent. The command-line tool here automates that process.

## Calculate what tokens should be sent

List the differences between a list of expected distributions and the record of what
transactions have already been sent.

```bash
solana-tokens distribute --dollars-per-sol <NUMBER> --dry-run <ALLOCATIONS_CSV> <TRANSACTION_LOG>
```

Example output:

```text
`**Recipient**`                                     `**Amount**`
6Vo87BaDhp4v4GHwVDhw5huhxVF8CyxSXYtkUwVHbbPv  70
3ihfUy1n9gaqihM5bJCiTAGLgWc5zo3DqVUS6T736NLM  42
UKUcTXgbeTYh65RaVV5gSf6xBHevqHvAXMo3e8Q6np8k  43
```

## Distribute tokens

Send tokens to the recipients in `<ALLOCATIONS_CSV>` if the distribution is
not already recordered in the transaction log.

```bash
solana-tokens distribute --from <KEYPAIR> --dollars-per-sol <NUMBER> <ALLOCATIONS_CSV> <TRANSACTION_LOG> --fee-payer <KEYPAIR>
```

Example output:

```text
`**Recipient**`                                     `**Amount**`
6Vo87BaDhp4v4GHwVDhw5huhxVF8CyxSXYtkUwVHbbPv  70
3ihfUy1n9gaqihM5bJCiTAGLgWc5zo3DqVUS6T736NLM  42
UKUcTXgbeTYh65RaVV5gSf6xBHevqHvAXMo3e8Q6np8k  43
```

Example transaction log before:

```text
recipient,amount,signature
6Vo87BaDhp4v4GHwVDhw5huhxVF8CyxSXYtkUwVHbbPv,30,orig
```

Example transaction log after:

```text
recipient,amount,signature
6Vo87BaDhp4v4GHwVDhw5huhxVF8CyxSXYtkUwVHbbPv,30,1111111111111111111111111111111111111111111111111111111111111111
6Vo87BaDhp4v4GHwVDhw5huhxVF8CyxSXYtkUwVHbbPv,70,1111111111111111111111111111111111111111111111111111111111111111
3ihfUy1n9gaqihM5bJCiTAGLgWc5zo3DqVUS6T736NLM,42,1111111111111111111111111111111111111111111111111111111111111111
UKUcTXgbeTYh65RaVV5gSf6xBHevqHvAXMo3e8Q6np8k,43,1111111111111111111111111111111111111111111111111111111111111111
```
