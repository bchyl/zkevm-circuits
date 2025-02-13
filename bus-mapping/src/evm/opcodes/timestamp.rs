#[cfg(test)]
mod timestamp_tests {
    use crate::{
        circuit_input_builder::{ExecStep, TransactionContext},
        mock::BlockData,
        operation::RW,
        Error,
    };
    use eth_types::{bytecode, evm_types::StackAddress};
    use mock::new_single_tx_trace_code_at_start;
    use pretty_assertions::assert_eq;

    #[test]
    fn timestamp_opcode_impl() -> Result<(), Error> {
        let code = bytecode! {
            #[start]
            TIMESTAMP
            STOP
        };

        // Get the execution steps from the external tracer
        let block =
            BlockData::new_from_geth_data(new_single_tx_trace_code_at_start(&code).unwrap());

        let mut builder = block.new_circuit_input_builder();
        builder.handle_tx(&block.eth_tx, &block.geth_trace).unwrap();

        let mut test_builder = block.new_circuit_input_builder();
        let mut tx = test_builder
            .new_tx(&block.eth_tx, !block.geth_trace.failed)
            .unwrap();
        let mut tx_ctx = TransactionContext::new(&block.eth_tx, &block.geth_trace).unwrap();

        // Generate step corresponding to TIMESTAMP
        let mut step = ExecStep::new(
            &block.geth_trace.struct_logs[0],
            0,
            test_builder.block_ctx.rwc,
            0,
        );
        let mut state_ref = test_builder.state_ref(&mut tx, &mut tx_ctx, &mut step);

        // Add the last Stack write
        state_ref.push_stack_op(
            RW::WRITE,
            StackAddress::from(1024 - 1),
            block.eth_block.timestamp,
        );

        tx.steps_mut().push(step);
        test_builder.block.txs_mut().push(tx);

        // Compare first step bus mapping instance
        assert_eq!(
            builder.block.txs()[0].steps()[0].bus_mapping_instance,
            test_builder.block.txs()[0].steps()[0].bus_mapping_instance,
        );

        // Compare containers
        assert_eq!(builder.block.container, test_builder.block.container);

        Ok(())
    }
}
