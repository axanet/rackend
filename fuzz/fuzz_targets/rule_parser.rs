#![no_main]
//! The rule parser must never panic, and printing a parsed tree must parse
//! back to the same tree.

use libfuzzer_sys::fuzz_target;
use rackend_core::rules::parser::{MAX_RULE_LEN, parse};

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data)
        && let Ok(ast) = parse(src, MAX_RULE_LEN)
    {
        let printed = ast.to_string();
        let again = parse(&printed, MAX_RULE_LEN).expect("printed expression must parse");
        assert_eq!(again, ast);
    }
});
