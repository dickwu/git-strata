fn main() {
    println!(
        "{}",
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "store_schema": strata_types::STORE_SCHEMA,
            "downgrade_floor": strata_types::DOWNGRADE_FLOOR,
        })
    );
}
