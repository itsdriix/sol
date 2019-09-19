use crate::instruction::{AccountMeta, Instruction};
use crate::instruction_processor_utils::DecodeError;
use crate::pubkey::Pubkey;
use crate::system_program;
use crate::sysvar::rent;
use num_derive::FromPrimitive;

#[derive(Serialize, Debug, Clone, PartialEq, FromPrimitive)]
pub enum SystemError {
    AccountAlreadyInUse,
    ResultWithNegativeLamports,
    SourceNotSystemAccount,
    InvalidProgramId,
    InvalidAccountId,
    InsufficientFunds,
}

impl<T> DecodeError<T> for SystemError {
    fn type_of() -> &'static str {
        "SystemError"
    }
}

impl std::fmt::Display for SystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "error")
    }
}
impl std::error::Error for SystemError {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SystemInstruction {
    /// Create a new account
    /// * Transaction::keys[0] - source
    /// * Transaction::keys[1] - new account key
    /// * Transaction::keys[2] - rent sysvar account key (Only required if require_rent_exemption is true)
    /// * lamports - number of lamports to transfer to the new account
    /// * space - memory to allocate if greater then zero
    /// * program_id - the program id of the new account
    /// * require_rent_exemption - if set to true, only allow account creation if it's rent exempt
    CreateAccount {
        lamports: u64,
        space: u64,
        program_id: Pubkey,
        require_rent_exemption: bool,
    },
    /// Assign account to a program
    /// * Transaction::keys[0] - account to assign
    Assign { program_id: Pubkey },
    /// Transfer lamports
    /// * Transaction::keys[0] - source
    /// * Transaction::keys[1] - destination
    Transfer { lamports: u64 },
}

pub fn create_account(
    from_pubkey: &Pubkey,
    to_pubkey: &Pubkey,
    lamports: u64,
    space: u64,
    program_id: &Pubkey,
) -> Instruction {
    generate_create_account_instruction(from_pubkey, to_pubkey, lamports, space, program_id, false)
}

pub fn create_rent_exempted_account(
    from_pubkey: &Pubkey,
    to_pubkey: &Pubkey,
    lamports: u64,
    space: u64,
    program_id: &Pubkey,
) -> Instruction {
    generate_create_account_instruction(from_pubkey, to_pubkey, lamports, space, program_id, true)
}

fn generate_create_account_instruction(
    from_pubkey: &Pubkey,
    to_pubkey: &Pubkey,
    lamports: u64,
    space: u64,
    program_id: &Pubkey,
    require_rent_exemption: bool,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(*from_pubkey, true),
        AccountMeta::new(*to_pubkey, false),
    ];

    if require_rent_exemption {
        account_metas.push(AccountMeta::new(rent::id(), false));
    }

    Instruction::new(
        system_program::id(),
        &SystemInstruction::CreateAccount {
            lamports,
            space,
            program_id: *program_id,
            require_rent_exemption,
        },
        account_metas,
    )
}

/// Create and sign a transaction to create a system account
pub fn create_user_account(from_pubkey: &Pubkey, to_pubkey: &Pubkey, lamports: u64) -> Instruction {
    let program_id = system_program::id();
    create_account(from_pubkey, to_pubkey, lamports, 0, &program_id)
}

pub fn assign(from_pubkey: &Pubkey, program_id: &Pubkey) -> Instruction {
    let account_metas = vec![AccountMeta::new(*from_pubkey, true)];
    Instruction::new(
        system_program::id(),
        &SystemInstruction::Assign {
            program_id: *program_id,
        },
        account_metas,
    )
}

pub fn transfer(from_pubkey: &Pubkey, to_pubkey: &Pubkey, lamports: u64) -> Instruction {
    let account_metas = vec![
        AccountMeta::new(*from_pubkey, true),
        AccountMeta::new_credit_only(*to_pubkey, false),
    ];
    Instruction::new(
        system_program::id(),
        &SystemInstruction::Transfer { lamports },
        account_metas,
    )
}

/// Create and sign new SystemInstruction::Transfer transaction to many destinations
pub fn transfer_many(from_pubkey: &Pubkey, to_lamports: &[(Pubkey, u64)]) -> Vec<Instruction> {
    to_lamports
        .iter()
        .map(|(to_pubkey, lamports)| transfer(from_pubkey, to_pubkey, *lamports))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_keys(instruction: &Instruction) -> Vec<Pubkey> {
        instruction.accounts.iter().map(|x| x.pubkey).collect()
    }

    #[test]
    fn test_move_many() {
        let alice_pubkey = Pubkey::new_rand();
        let bob_pubkey = Pubkey::new_rand();
        let carol_pubkey = Pubkey::new_rand();
        let to_lamports = vec![(bob_pubkey, 1), (carol_pubkey, 2)];

        let instructions = transfer_many(&alice_pubkey, &to_lamports);
        assert_eq!(instructions.len(), 2);
        assert_eq!(get_keys(&instructions[0]), vec![alice_pubkey, bob_pubkey]);
        assert_eq!(get_keys(&instructions[1]), vec![alice_pubkey, carol_pubkey]);
    }
}
