//! Container to capture information relevant to computing a bank hash

use {
    super::Bank,
    base64::{prelude::BASE64_STANDARD, Engine},
    serde::{
        de::{self, Deserialize, Deserializer},
        ser::{Serialize, SerializeSeq, Serializer},
    },
    solana_sdk::{
        account::{Account, AccountSharedData, ReadableAccount},
        clock::{Epoch, Slot},
        hash::Hash,
        pubkey::Pubkey,
    },
    std::str::FromStr,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct BankHashDetails {
    /// client version
    pub version: String,
    pub account_data_encoding: String,
    pub slot: Slot,
    pub bank_hash: String,
    pub parent_bank_hash: String,
    pub accounts_delta_hash: String,
    pub signature_count: u64,
    pub last_blockhash: String,
    pub accounts: BankHashAccounts,
}

impl BankHashDetails {
    pub fn new(
        slot: Slot,
        bank_hash: Hash,
        parent_bank_hash: Hash,
        accounts_delta_hash: Hash,
        signature_count: u64,
        last_blockhash: Hash,
        accounts: BankHashAccounts,
    ) -> Self {
        Self {
            version: solana_version::version!().to_string(),
            account_data_encoding: "base64".to_string(),
            slot,
            bank_hash: bank_hash.to_string(),
            parent_bank_hash: parent_bank_hash.to_string(),
            accounts_delta_hash: accounts_delta_hash.to_string(),
            signature_count,
            last_blockhash: last_blockhash.to_string(),
            accounts,
        }
    }
}

impl TryFrom<&Bank> for BankHashDetails {
    type Error = String;

    fn try_from(bank: &Bank) -> Result<Self, Self::Error> {
        let slot = bank.slot();
        if !bank.is_frozen() {
            return Err(format!(
                "Bank {slot} must be frozen in order to get bank hash details"
            ));
        }

        // This bank is frozen; as a result, we know that the state has been
        // hashed which means the delta hash is Some(). So, .unwrap() is safe
        let accounts_delta_hash = bank
            .rc
            .accounts
            .accounts_db
            .get_accounts_delta_hash(slot)
            .unwrap()
            .0;
        let mut accounts = bank
            .rc
            .accounts
            .accounts_db
            .get_pubkey_hash_account_for_slot(slot);
        // get_pubkey_hash_account_for_slot() returns an arbitrary ordering;
        // sort by pubkey to match the ordering used for accounts delta hash
        accounts.sort_by_key(|(pubkey, _, _)| *pubkey);

        Ok(Self::new(
            slot,
            bank.hash(),
            bank.parent_hash(),
            accounts_delta_hash,
            bank.signature_count(),
            bank.last_blockhash(),
            BankHashAccounts(accounts),
        ))
    }
}

// Wrap the Vec<...> so we can implement custom Serialize/Deserialize traits on the wrapper type
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BankHashAccounts(pub Vec<(Pubkey, Hash, AccountSharedData)>);

#[derive(Deserialize, Serialize)]
/// Used as an intermediate for serializing and deserializing account fields
/// into a human readable format.
struct TempAccount {
    pubkey: String,
    hash: String,
    owner: String,
    lamports: u64,
    rent_epoch: Epoch,
    executable: bool,
    data: String,
}

impl Serialize for BankHashAccounts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (pubkey, hash, account) in self.0.iter() {
            let temp = TempAccount {
                pubkey: pubkey.to_string(),
                hash: hash.to_string(),
                owner: account.owner().to_string(),
                lamports: account.lamports(),
                rent_epoch: account.rent_epoch(),
                executable: account.executable(),
                data: BASE64_STANDARD.encode(account.data()),
            };
            seq.serialize_element(&temp)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for BankHashAccounts {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let temp_accounts: Vec<TempAccount> = Deserialize::deserialize(deserializer)?;
        let pubkey_hash_accounts: Result<Vec<_>, _> = temp_accounts
            .into_iter()
            .map(|temp_account| {
                let pubkey = Pubkey::from_str(&temp_account.pubkey).map_err(de::Error::custom)?;
                let hash = Hash::from_str(&temp_account.hash).map_err(de::Error::custom)?;
                let account = AccountSharedData::from(Account {
                    lamports: temp_account.lamports,
                    data: BASE64_STANDARD
                        .decode(temp_account.data)
                        .map_err(de::Error::custom)?,
                    owner: Pubkey::from_str(&temp_account.owner).map_err(de::Error::custom)?,
                    executable: temp_account.executable,
                    rent_epoch: temp_account.rent_epoch,
                });
                Ok((pubkey, hash, account))
            })
            .collect();
        let pubkey_hash_accounts = pubkey_hash_accounts?;
        Ok(BankHashAccounts(pubkey_hash_accounts))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_serde_bank_hash_details() {
        use solana_sdk::hash::hash;

        let slot = 123_456_789;
        let signature_count = 314;

        let account = AccountSharedData::from(Account {
            lamports: 123_456_789,
            data: vec![0, 9, 1, 8, 2, 7, 3, 6, 4, 5],
            owner: Pubkey::new_unique(),
            executable: true,
            rent_epoch: 123,
        });
        let account_pubkey = Pubkey::new_unique();
        let account_hash = hash("account".as_bytes());
        let accounts = BankHashAccounts(vec![(account_pubkey, account_hash, account)]);

        let bank_hash = hash("bank".as_bytes());
        let parent_bank_hash = hash("parent_bank".as_bytes());
        let accounts_delta_hash = hash("accounts_delta".as_bytes());
        let last_blockhash = hash("last_blockhash".as_bytes());

        let bank_hash_details = BankHashDetails::new(
            slot,
            bank_hash,
            parent_bank_hash,
            accounts_delta_hash,
            signature_count,
            last_blockhash,
            accounts,
        );

        let serialized_bytes = serde_json::to_vec(&bank_hash_details).unwrap();
        let deserialized_bank_hash_details: BankHashDetails =
            serde_json::from_slice(&serialized_bytes).unwrap();

        assert_eq!(bank_hash_details, deserialized_bank_hash_details);
    }
}
