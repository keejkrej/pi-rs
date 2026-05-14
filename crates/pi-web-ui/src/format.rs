use pi_agent_core::messages::Usage;

pub fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

pub fn format_cost(cost: f64) -> String {
    if cost == 0.0 {
        "$0.00".to_string()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

pub fn format_usage(usage: &Usage) -> String {
    format!(
        "{} in / {} out / {} total / {}",
        format_token_count(usage.input),
        format_token_count(usage.output),
        format_token_count(usage.total_tokens),
        format_cost(usage.cost.total),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_tokens() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_250), "1.2k");
    }
}
