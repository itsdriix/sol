use super::*;

declare_builtin_function!(
    SyscallIsFeatureActive,
    fn rust(
        invoke_context: &mut InvokeContext,
        var_addr: u64,
        feature_pubkey_addr: u64,
        _arg3: u64,
        _arg4: u64,
        _arg5: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Error> {
        consume_compute_meter(
            invoke_context,
            invoke_context
                .get_compute_budget()
                .sysvar_base_cost // XXX something else?
                .saturating_add(size_of::<bool>() as u64),
        )?;

        let feature_pubkey = translate_type_mut::<Pubkey>(
            memory_mapping,
            feature_pubkey_addr,
            invoke_context.get_check_aligned(),
        )?;

        // XXX TODO FIXME im 80% sure we can get rid of this and pass in a bool pointer
        // but im in toolchain hell rn after rebasing 1.17 -> 1.18 so i cant test the change
        let var = translate_type_mut::<bool>(
            memory_mapping,
            var_addr,
            invoke_context.get_check_aligned(),
        )?;
        *var = invoke_context.feature_set.is_active(&feature_pubkey);

        Ok(SUCCESS)
    }
);