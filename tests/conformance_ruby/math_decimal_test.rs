//! Original cases checked against Shopify Liquid 5.14.0 (4e39ae4).
//! Decimal operands must avoid binary multiplication roundoff.

use liquid_core::{Value, ValueView};
use liquid_lib::stdlib::Times;

#[test]
fn times_multiplies_decimal_representations() {
    for (input, operand, expected) in [
        (Value::scalar(3.5), Value::scalar(1.6), 5.6),
        (Value::scalar(3.0), Value::scalar(1.6), 4.8),
        (Value::scalar(3), Value::scalar(1.6), 4.8),
        (Value::scalar(1.6), Value::scalar(3), 4.8),
        (Value::scalar(0.1), Value::scalar(0.2), 0.02),
        (Value::scalar(-3.5), Value::scalar(1.6), -5.6),
        (Value::scalar("3.5"), Value::scalar("1.6"), 5.6),
        (Value::scalar(0.00003), Value::scalar(0.00006), 1.8e-9),
        (
            Value::scalar(9_007_199_254_740_993i64),
            Value::scalar(0.1),
            900_719_925_474_099.2,
        ),
    ] {
        let actual = liquid_core::call_filter!(Times, input, operand).unwrap();
        assert_eq!(actual.as_scalar().unwrap().to_float(), Some(expected));
    }
}

#[test]
fn times_renders_decimal_products_without_binary_roundoff() {
    assert_template_result!(
        "5.6|4.8|4.0|0.02|-5.6|5.6|56|0.0|-0.0",
        "{{ 3.5 | times: 1.6 }}|{{ 3.0 | times: 1.6 }}|{{ 2.5 | times: 1.6 }}|{{ 0.1 | times: 0.2 }}|{{ -3.5 | times: 1.6 }}|{{ '3.5' | times: '1.6' }}|{{ 7 | times: 8 }}|{{ nil | times: 1.6 }}|{{ -2.5 | times: nil }}",
    );
}

#[test]
fn times_preserves_integer_and_float_result_types() {
    let integer = liquid_core::call_filter!(Times, 7, 8).unwrap();
    assert_eq!(integer.as_scalar().unwrap().to_integer(), Some(56));
    let integer_strings = liquid_core::call_filter!(Times, "7", "8").unwrap();
    assert_eq!(integer_strings.as_scalar().unwrap().to_integer(), Some(56));
    let float = liquid_core::call_filter!(Times, 2.5, 1.6).unwrap();
    assert_eq!(float.as_scalar().unwrap().to_integer(), None);
    assert_eq!(float.render().to_string(), "4.0");
}

#[test]
fn times_preserves_zero_signs_and_non_finite_results() {
    for (input, operand, expected) in [
        (0.0, 1.6, 0.0),
        (-0.0, 1.6, -0.0),
        (-0.0, -1.6, 0.0),
        (0.0, -1.6, -0.0),
        (1e-200, 1e-200, 0.0),
        (-1e-200, 1e-200, -0.0),
        (1e200, 1e200, f64::INFINITY),
        (f64::INFINITY, 1.6, f64::INFINITY),
        (f64::NEG_INFINITY, 1.6, f64::NEG_INFINITY),
    ] {
        let actual = liquid_core::call_filter!(Times, input, operand)
            .unwrap()
            .as_scalar()
            .unwrap()
            .to_float()
            .unwrap();
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
    for (input, operand) in [(f64::NAN, 1.6), (0.0, f64::INFINITY)] {
        let actual = liquid_core::call_filter!(Times, input, operand)
            .unwrap()
            .as_scalar()
            .unwrap()
            .to_float()
            .unwrap();
        assert!(actual.is_nan());
    }
}

#[test]
fn times_retains_invalid_number_and_missing_operand_errors() {
    liquid_core::call_filter!(Times, true, 1.6).unwrap_err();
    liquid_core::call_filter!(Times, 1.6, true).unwrap_err();
    liquid_core::call_filter!(Times, "not a number", 1.6).unwrap_err();
    liquid_core::call_filter!(Times, 1.6).unwrap_err();
}

#[test]
fn times_uses_canonical_float_decimals_without_rounding_string_operands() {
    let input = f64::from_bits(0x4050_dd72_327c_bf13);
    let operand = f64::from_bits(0xc300_4a1b_8ed9_24a2);
    for (input, operand, expected_bits) in [
        (
            Value::scalar(input),
            Value::scalar(operand),
            0xc361_2b8f_6ee9_2cb3,
        ),
        (
            Value::scalar(operand),
            Value::scalar(input),
            0xc361_2b8f_6ee9_2cb3,
        ),
        (
            Value::scalar(input),
            Value::scalar("-573135231067284.2"),
            0xc361_2b8f_6ee9_2cb3,
        ),
        (
            Value::scalar(input),
            Value::scalar("-573135231067284.3"),
            0xc361_2b8f_6ee9_2cb4,
        ),
    ] {
        let actual = liquid_core::call_filter!(Times, input, operand)
            .unwrap()
            .as_scalar()
            .unwrap()
            .to_float()
            .unwrap();
        assert_eq!(actual.to_bits(), expected_bits);
    }
}
