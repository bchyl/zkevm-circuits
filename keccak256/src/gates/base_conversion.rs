use halo2_proofs::{
    circuit::{AssignedCell, Layouter},
    plonk::{Advice, Column, ConstraintSystem, Error, Selector},
    poly::Rotation,
};

use crate::gates::tables::BaseInfo;
use eth_types::Field;

#[derive(Clone, Debug)]
pub(crate) struct BaseConversionConfig<F> {
    q_running_sum: Selector,
    q_lookup: Selector,
    base_info: BaseInfo<F>,
    // Flag is copied from the parent flag. Parent flag is assumed to be binary
    // constrained.
    flag: Column<Advice>,
    input_lane: Column<Advice>,
    input_coef: Column<Advice>,
    input_acc: Column<Advice>,
    output_coef: Column<Advice>,
    output_acc: Column<Advice>,
}

impl<F: Field> BaseConversionConfig<F> {
    /// Side effect: lane and parent_flag is equality enabled
    pub(crate) fn configure(
        meta: &mut ConstraintSystem<F>,
        base_info: BaseInfo<F>,
        input_lane: Column<Advice>,
        parent_flag: Column<Advice>,
    ) -> Self {
        let q_running_sum = meta.selector();
        let q_lookup = meta.complex_selector();
        let flag = meta.advice_column();
        let input_coef = meta.advice_column();
        let input_acc = meta.advice_column();
        let output_coef = meta.advice_column();
        let output_acc = meta.advice_column();

        meta.enable_equality(flag);
        meta.enable_equality(input_coef);
        meta.enable_equality(input_acc);
        meta.enable_equality(output_coef);
        meta.enable_equality(output_acc);
        meta.enable_equality(input_lane);
        meta.enable_equality(parent_flag);

        meta.create_gate("input running sum", |meta| {
            let q_enable = meta.query_selector(q_running_sum);
            let flag = meta.query_advice(flag, Rotation::cur());
            let coef = meta.query_advice(input_coef, Rotation::cur());
            let acc_prev = meta.query_advice(input_acc, Rotation::prev());
            let acc = meta.query_advice(input_acc, Rotation::cur());
            let power_of_base = base_info.input_pob();
            vec![q_enable * flag * (acc - acc_prev * power_of_base - coef)]
        });
        meta.create_gate("output running sum", |meta| {
            let q_enable = meta.query_selector(q_running_sum);
            let flag = meta.query_advice(flag, Rotation::cur());
            let coef = meta.query_advice(output_coef, Rotation::cur());
            let acc_prev = meta.query_advice(output_acc, Rotation::prev());
            let acc = meta.query_advice(output_acc, Rotation::cur());
            let power_of_base = base_info.output_pob();
            vec![q_enable * flag * (acc - acc_prev * power_of_base - coef)]
        });
        meta.lookup("Lookup i/o_coeff at Base conversion table", |meta| {
            let q_enable = meta.query_selector(q_lookup);
            let flag = meta.query_advice(flag, Rotation::cur());
            let input_slices = meta.query_advice(input_coef, Rotation::cur());
            let output_slices = meta.query_advice(output_coef, Rotation::cur());
            vec![
                (
                    q_enable.clone() * flag.clone() * input_slices,
                    base_info.input_tc,
                ),
                (q_enable * flag * output_slices, base_info.output_tc),
            ]
        });

        Self {
            q_running_sum,
            q_lookup,
            base_info,
            flag,
            input_lane,
            input_coef,
            input_acc,
            output_coef,
            output_acc,
        }
    }

    pub(crate) fn assign_region(
        &self,
        layouter: &mut impl Layouter<F>,
        input: AssignedCell<F, F>,
        flag: AssignedCell<F, F>,
    ) -> Result<AssignedCell<F, F>, Error> {
        // TODO: Add propper err handling once AssignedCell has a better API for it.
        let (input_coefs, output_coefs, _) = self
            .base_info
            .compute_coefs(*input.value().unwrap_or(&F::zero()))?;

        layouter.assign_region(
            || "Base conversion",
            |mut region| {
                let mut input_acc = F::zero();
                let input_pob = self.base_info.input_pob();
                let mut output_acc = F::zero();
                let output_pob = self.base_info.output_pob();
                for (offset, (&input_coef, &output_coef)) in
                    input_coefs.iter().zip(output_coefs.iter()).enumerate()
                {
                    self.q_lookup.enable(&mut region, offset)?;
                    if offset != 0 {
                        self.q_running_sum.enable(&mut region, offset)?;
                    }
                    flag.copy_advice(|| "Base conv flag", &mut region, self.flag, offset)?;

                    let input_coef_cell = region.assign_advice(
                        || "Input Coef",
                        self.input_coef,
                        offset,
                        || Ok(input_coef),
                    )?;
                    input_acc = input_acc * input_pob + input_coef;
                    let input_acc_cell = region.assign_advice(
                        || "Input Acc",
                        self.input_acc,
                        offset,
                        || Ok(input_acc),
                    )?;
                    let output_coef_cell = region.assign_advice(
                        || "Output Coef",
                        self.output_coef,
                        offset,
                        || Ok(output_coef),
                    )?;
                    output_acc = output_acc * output_pob + output_coef;
                    let output_acc_cell = region.assign_advice(
                        || "Output Acc",
                        self.output_acc,
                        offset,
                        || Ok(output_acc),
                    )?;

                    if offset == 0 {
                        // bind first acc to first coef
                        region.constrain_equal(input_acc_cell.cell(), input_coef_cell.cell())?;
                        region.constrain_equal(output_acc_cell.cell(), output_coef_cell.cell())?;
                    } else if offset == input_coefs.len() - 1 {
                        //region.constrain_equal(input_acc_cell, input.0)?;
                        return Ok(output_acc_cell);
                    }
                }
                unreachable!();
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arith_helpers::{convert_b2_to_b13, convert_b9_lane_to_b13};
    use crate::gates::{
        gate_helpers::biguint_to_f,
        tables::{FromBase9TableConfig, FromBinaryTableConfig},
    };
    use halo2_proofs::{
        circuit::{Layouter, SimpleFloorPlanner},
        dev::MockProver,
        plonk::{Advice, Circuit, Column, ConstraintSystem, Error},
    };
    use num_bigint::BigUint;
    use pairing::bn256::Fr as Fp;
    use pretty_assertions::assert_eq;
    #[test]
    fn test_base_conversion_from_b2() {
        // We have to use a MyConfig because:
        // We need to load the table
        #[derive(Debug, Clone)]
        struct MyConfig<F> {
            lane: Column<Advice>,
            flag: Column<Advice>,
            table: FromBinaryTableConfig<F>,
            conversion: BaseConversionConfig<F>,
        }
        impl<F: Field> MyConfig<F> {
            pub fn configure(meta: &mut ConstraintSystem<F>) -> Self {
                let table = FromBinaryTableConfig::configure(meta);
                let lane = meta.advice_column();
                let flag = meta.advice_column();
                let base_info = table.get_base_info(false);
                let conversion = BaseConversionConfig::configure(meta, base_info, lane, flag);
                Self {
                    lane,
                    flag,
                    table,
                    conversion,
                }
            }

            pub fn load(&self, layouter: &mut impl Layouter<F>) -> Result<(), Error> {
                self.table.load(layouter)
            }

            pub fn assign_region(
                &self,
                layouter: &mut impl Layouter<F>,
                input: F,
            ) -> Result<F, Error> {
                // The main flag is enabled
                let flag_value = F::one();
                let (lane, flag) = layouter.assign_region(
                    || "Input lane",
                    |mut region| {
                        let lane =
                            region.assign_advice(|| "Input lane", self.lane, 0, || Ok(input))?;
                        let flag = region.assign_advice(
                            || "main flag",
                            self.flag,
                            0,
                            || Ok(flag_value),
                        )?;
                        Ok((lane, flag))
                    },
                )?;
                let output = self.conversion.assign_region(layouter, lane, flag)?;
                layouter.assign_region(
                    || "Input lane",
                    |mut region| output.copy_advice(|| "Output lane", &mut region, self.lane, 0),
                )?;
                // TODO: Handle this better once AssignedCell has the API to do so
                Ok(*output
                    .value()
                    .unwrap_or(&F::from_u128(0x22c268c05977fd626636ccu128)))
            }
        }

        #[derive(Default)]
        struct MyCircuit<F> {
            input_b2_lane: F,
            output_b13_lane: F,
        }
        impl<F: Field> Circuit<F> for MyCircuit<F> {
            type Config = MyConfig<F>;
            type FloorPlanner = SimpleFloorPlanner;

            fn without_witnesses(&self) -> Self {
                Self::default()
            }

            fn configure(meta: &mut ConstraintSystem<F>) -> Self::Config {
                Self::Config::configure(meta)
            }

            fn synthesize(
                &self,
                config: Self::Config,
                mut layouter: impl Layouter<F>,
            ) -> Result<(), Error> {
                config.load(&mut layouter)?;
                let output = config.assign_region(&mut layouter, self.input_b2_lane)?;
                assert_eq!(output, self.output_b13_lane);
                Ok(())
            }
        }
        let input = 12345678u64;
        let circuit = MyCircuit::<Fp> {
            input_b2_lane: Fp::from(input),
            output_b13_lane: biguint_to_f::<Fp>(&convert_b2_to_b13(input)),
        };
        let k = 17;

        #[cfg(feature = "dev-graph")]
        {
            use plotters::prelude::*;
            let root = BitMapBackend::new("base-conversion.png", (1024, 32768)).into_drawing_area();
            root.fill(&WHITE).unwrap();
            let root = root.titled("Base conversion", ("sans-serif", 60)).unwrap();
            halo2_proofs::dev::CircuitLayout::default()
                .mark_equality_cells(true)
                .render(k, &circuit, &root)
                .unwrap();
        }
        let prover = MockProver::<Fp>::run(k, &circuit, vec![]).unwrap();
        assert_eq!(prover.verify(), Ok(()));
    }

    #[test]
    fn test_base_conversion_from_b9() {
        #[derive(Debug, Clone)]
        struct MyConfig<F> {
            lane: Column<Advice>,
            flag: Column<Advice>,
            table: FromBase9TableConfig<F>,
            conversion: BaseConversionConfig<F>,
        }
        impl<F: Field> MyConfig<F> {
            pub fn configure(meta: &mut ConstraintSystem<F>) -> Self {
                let table = FromBase9TableConfig::configure(meta);
                let lane = meta.advice_column();
                let flag = meta.advice_column();
                let base_info = table.get_base_info(false);
                let conversion = BaseConversionConfig::configure(meta, base_info, lane, flag);
                Self {
                    lane,
                    flag,
                    table,
                    conversion,
                }
            }

            pub fn load(&self, layouter: &mut impl Layouter<F>) -> Result<(), Error> {
                self.table.load(layouter)
            }

            pub fn assign_region(
                &self,
                layouter: &mut impl Layouter<F>,
                input: F,
            ) -> Result<F, Error> {
                // The main flag is enabled
                let flag_value = F::one();
                let (lane, flag) = layouter.assign_region(
                    || "Input lane",
                    |mut region| {
                        let lane =
                            region.assign_advice(|| "Input lane", self.lane, 0, || Ok(input))?;
                        let flag = region.assign_advice(
                            || "main flag",
                            self.flag,
                            0,
                            || Ok(flag_value),
                        )?;
                        Ok((lane, flag))
                    },
                )?;

                let output = self.conversion.assign_region(layouter, lane, flag)?;
                layouter.assign_region(
                    || "Input lane",
                    |mut region| output.copy_advice(|| "Output lane", &mut region, self.lane, 0),
                )?;

                Ok(*output.value().expect("Add propper err handling"))
            }
        }

        #[derive(Default)]
        struct MyCircuit<F> {
            input_lane: F,
            output_lane: F,
        }
        impl<F: Field> Circuit<F> for MyCircuit<F> {
            type Config = MyConfig<F>;
            type FloorPlanner = SimpleFloorPlanner;

            fn without_witnesses(&self) -> Self {
                Self::default()
            }

            fn configure(meta: &mut ConstraintSystem<F>) -> Self::Config {
                Self::Config::configure(meta)
            }

            fn synthesize(
                &self,
                config: Self::Config,
                mut layouter: impl Layouter<F>,
            ) -> Result<(), Error> {
                config.load(&mut layouter)?;
                let output = config.assign_region(&mut layouter, self.input_lane)?;
                assert_eq!(output, self.output_lane);
                Ok(())
            }
        }
        let input = BigUint::parse_bytes(b"02939a42ef593e37757abe328e9e409e75dcd76cf1b3427bc3", 16)
            .unwrap();
        let circuit = MyCircuit::<Fp> {
            input_lane: biguint_to_f::<Fp>(&input),
            output_lane: biguint_to_f::<Fp>(&convert_b9_lane_to_b13(input)),
        };
        let k = 17;
        let prover = MockProver::<Fp>::run(k, &circuit, vec![]).unwrap();
        assert_eq!(prover.verify(), Ok(()));
    }
}
