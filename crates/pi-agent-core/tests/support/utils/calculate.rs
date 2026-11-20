//! Port of packages/agent/test/utils/calculate.ts

use std::sync::Arc;

use pi_ai::{Usage, UserContentBlock, faux_text};
use pi_js::{Error, Result};
use pi_schema::{Annotations, ObjOptions, StrOptions, obj_with, str_with};
use serde_json::Value;

use pi_agent_core::{AgentTool, AgentToolResult};

pub fn calculate(expression: &str) -> Result<AgentToolResult> {
    let value = eval_expression(expression)?;
    let formatted = format_number(value);
    Ok(AgentToolResult {
        content: vec![UserContentBlock::Text(faux_text(&format!(
            "{expression} = {formatted}"
        )))],
        details: None,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
        control: None,
    })
}

pub fn calculate_tool() -> AgentTool {
    let parameters = obj_with(
        [(
            "expression",
            str_with(StrOptions {
                annotations: Annotations {
                    description: Some("The mathematical expression to evaluate".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )],
        ObjOptions {
            open: true,
            ..Default::default()
        },
    );

    AgentTool {
        name: "calculate".to_string(),
        label: "Calculator".to_string(),
        description: "Evaluate mathematical expressions".to_string(),
        parameters,
        constrained_sampling: None,
        output_schema: None,
        execution_mode: None,
        prepare_arguments: None,
        execute: Arc::new(|_tool_call_id, args, _signal, _on_update| {
            Box::pin(async move {
                let expr = args
                    .get("expression")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::msg("Missing string argument `expression`"))?;
                calculate(expr)
            })
        }),
        replay: None,
    }
}

pub fn create_calculate_tool_with_usage(usage: Usage) -> AgentTool {
    let mut tool = calculate_tool();
    tool.execute = Arc::new(move |_tool_call_id, args, _signal, _on_update| {
        let usage = usage.clone();
        Box::pin(async move {
            let expr = args
                .get("expression")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::msg("Missing string argument `expression`"))?;
            let mut result = calculate(expr)?;
            result.usage = Some(usage);
            Ok(result)
        })
    });
    tool
}

fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_991.0 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

fn eval_expression(input: &str) -> Result<f64> {
    let mut parser = ExprParser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    let value = parser.parse_expr()?;
    parser.skip_ws();
    if parser.pos < parser.bytes.len() {
        return Err(Error::msg(format!(
            "Unexpected token in expression near `{}`",
            &input[parser.pos..]
        )));
    }
    Ok(value)
}

struct ExprParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ExprParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn parse_expr(&mut self) -> Result<f64> {
        let mut lhs = self.parse_term()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.pos += 1;
            let rhs = self.parse_term()?;
            lhs = if op == b'+' { lhs + rhs } else { lhs - rhs };
        }
        Ok(lhs)
    }

    fn parse_term(&mut self) -> Result<f64> {
        let mut lhs = self.parse_unary()?;
        while let Some(op @ (b'*' | b'/' | b'%')) = self.peek() {
            self.pos += 1;
            let rhs = self.parse_unary()?;
            lhs = match op {
                b'*' => lhs * rhs,
                b'/' => lhs / rhs,
                _ => lhs % rhs,
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<f64> {
        match self.peek() {
            Some(b'+') => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(b'-') => {
                self.pos += 1;
                Ok(-self.parse_unary()?)
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<f64> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let value = self.parse_expr()?;
                if self.peek() != Some(b')') {
                    return Err(Error::msg("Missing closing parenthesis in expression"));
                }
                self.pos += 1;
                Ok(value)
            }
            Some(b'0'..=b'9' | b'.') => {
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'.')
                {
                    self.pos += 1;
                }
                if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
                    self.pos += 1;
                    if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'+' | b'-') {
                        self.pos += 1;
                    }
                    while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                }
                let slice = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| Error::msg("Invalid UTF-8 in number"))?;
                slice
                    .parse::<f64>()
                    .map_err(|_| Error::msg(format!("Invalid numeric literal `{slice}`")))
            }
            Some(ch) => Err(Error::msg(format!("Unexpected token `{}`", ch as char))),
            None => Err(Error::msg("Unexpected end of expression")),
        }
    }
}
