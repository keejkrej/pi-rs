//! Port of packages/coding-agent/src/cli/experimental/command.ts

use std::any::Any;
use std::fmt::Debug;
use std::sync::Arc;

use indexmap::IndexMap;

/// Result returned when parsing an individual command option value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandOptionParseResult<T> {
    Ok(T),
    Err(String),
}

/// Result returned by parsing a command invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandParseResult<T> {
    Ok { command: T },
    Err { errors: Vec<String> },
}

impl<T> CommandParseResult<T> {
    pub fn ok(command: T) -> Self {
        Self::Ok { command }
    }

    pub fn err(errors: Vec<String>) -> Self {
        Self::Err { errors }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn command(&self) -> Option<&T> {
        match self {
            Self::Ok { command } => Some(command),
            Self::Err { .. } => None,
        }
    }

    pub fn errors(&self) -> &[String] {
        match self {
            Self::Ok { .. } => &[],
            Self::Err { errors } => errors,
        }
    }
}

pub type CommandExecutionResult<T> = CommandParseResult<T>;
pub type CommandBuildResult<T> = CommandParseResult<T>;

type ParseErasedFn = Arc<dyn Fn(&str) -> Result<Box<dyn Any + Send + Sync>, String> + Send + Sync>;

/// Typed command-line option descriptor.
#[derive(Clone)]
pub struct CommandOption<T> {
    pub name: &'static str,
    pub flag: bool,
    pub repeatable: bool,
    parse_erased: ParseErasedFn,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: Send + Sync + 'static> CommandOption<T> {
    fn erased(&self) -> ErasedCommandOption {
        ErasedCommandOption {
            name: self.name,
            flag: self.flag,
            repeatable: self.repeatable,
            parse_erased: Arc::clone(&self.parse_erased),
        }
    }
}

#[derive(Clone)]
struct ErasedCommandOption {
    name: &'static str,
    flag: bool,
    repeatable: bool,
    parse_erased: ParseErasedFn,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OptionConfig {
    pub repeatable: bool,
}

pub fn value_option<T, F>(name: &'static str, parse: F, config: OptionConfig) -> CommandOption<T>
where
    T: Clone + Send + Sync + 'static,
    F: Fn(&str) -> CommandOptionParseResult<T> + Send + Sync + 'static,
{
    CommandOption {
        name,
        flag: false,
        repeatable: config.repeatable,
        parse_erased: Arc::new(move |value| match parse(value) {
            CommandOptionParseResult::Ok(parsed) => Ok(Box::new(parsed)),
            CommandOptionParseResult::Err(error) => Err(error),
        }),
        _marker: std::marker::PhantomData,
    }
}

pub fn string_option(name: &'static str, config: OptionConfig) -> CommandOption<String> {
    value_option(name, |value| CommandOptionParseResult::Ok(value.to_string()), config)
}

pub fn flag_option(name: &'static str) -> CommandOption<bool> {
    CommandOption {
        name,
        flag: true,
        repeatable: false,
        parse_erased: Arc::new(|_| Ok(Box::new(true))),
        _marker: std::marker::PhantomData,
    }
}

/// Parsed options and trailing positional/unrecognized arguments for a command builder.
pub struct ParsedCommandInput {
    values: IndexMap<String, Vec<Box<dyn Any + Send + Sync>>>,
    remaining_args: Vec<String>,
}

impl ParsedCommandInput {
    pub fn remaining_args(&self) -> &[String] {
        &self.remaining_args
    }

    pub fn value<T: Clone + 'static>(&self, option: &CommandOption<T>) -> Option<T> {
        self.values
            .get(option.name)
            .and_then(|list| list.first())
            .and_then(|boxed| boxed.downcast_ref::<T>())
            .cloned()
    }

    pub fn values<T: Clone + 'static>(&self, option: &CommandOption<T>) -> Vec<T> {
        self.values
            .get(option.name)
            .map(|list| {
                list.iter()
                    .filter_map(|boxed| boxed.downcast_ref::<T>().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

struct MutableParsedCommandInput {
    values: IndexMap<String, Vec<Box<dyn Any + Send + Sync>>>,
    remaining_args: Vec<String>,
    errors: Vec<String>,
}

type CommandBuilderFn<TInvocation> = Arc<dyn Fn(&ParsedCommandInput) -> CommandBuildResult<TInvocation> + Send + Sync>;

type CommandActionFn<TInvocation, TContext> =
    Arc<dyn Fn(TInvocation, &TContext) -> futures::future::BoxFuture<'_, pi_js::Result<()>> + Send + Sync>;

trait RegisteredSubcommand<TInvocation, TContext>: Send + Sync {
    fn parse(&self, argv: &[String]) -> CommandParseResult<TInvocation>;
    fn execute<'a>(
        &'a self,
        argv: &'a [String],
        context: &'a TContext,
    ) -> futures::future::BoxFuture<'a, pi_js::Result<CommandExecutionResult<TInvocation>>>;
}

struct MappedSubcommand<TSubOwn, TSubContext, TParentInvocation, TParentContext, FMapInv, FMapCtx> {
    inner: Command<TSubOwn, TSubContext>,
    map_invocation: FMapInv,
    map_context: FMapCtx,
    _marker: std::marker::PhantomData<fn(TParentInvocation, TParentContext)>,
}

impl<TSubOwn, TSubContext, TParentInvocation, TParentContext, FMapInv, FMapCtx>
    RegisteredSubcommand<TParentInvocation, TParentContext>
    for MappedSubcommand<TSubOwn, TSubContext, TParentInvocation, TParentContext, FMapInv, FMapCtx>
where
    TSubOwn: Clone + Send + Sync + 'static,
    TSubContext: Send + Sync + 'static,
    TParentInvocation: Clone + Send + Sync + 'static,
    TParentContext: Send + Sync + 'static,
    FMapInv: Fn(TSubOwn) -> TParentInvocation + Send + Sync,
    FMapCtx: Fn(&TParentContext) -> &TSubContext + Send + Sync,
{
    fn parse(&self, argv: &[String]) -> CommandParseResult<TParentInvocation> {
        match self.inner.parse(argv) {
            CommandParseResult::Ok { command } => CommandParseResult::Ok {
                command: (self.map_invocation)(command),
            },
            CommandParseResult::Err { errors } => CommandParseResult::Err { errors },
        }
    }

    fn execute<'a>(
        &'a self,
        argv: &'a [String],
        context: &'a TParentContext,
    ) -> futures::future::BoxFuture<'a, pi_js::Result<CommandExecutionResult<TParentInvocation>>> {
        let sub_ctx = (self.map_context)(context);
        Box::pin(async move {
            match self.inner.execute(argv, sub_ctx).await? {
                CommandParseResult::Ok { command } => Ok(CommandParseResult::Ok {
                    command: (self.map_invocation)(command),
                }),
                CommandParseResult::Err { errors } => Ok(CommandParseResult::Err { errors }),
            }
        })
    }
}

/// Composable CLI command definition matching TypeScript `Command`.
pub struct Command<TOwnInvocation, TContext> {
    pub name: String,
    options: IndexMap<&'static str, ErasedCommandOption>,
    subcommands: IndexMap<String, Arc<dyn RegisteredSubcommand<TOwnInvocation, TContext>>>,
    builder: Option<CommandBuilderFn<TOwnInvocation>>,
    command_action: Option<CommandActionFn<TOwnInvocation, TContext>>,
}

impl<TOwnInvocation, TContext> Command<TOwnInvocation, TContext>
where
    TOwnInvocation: Clone + Send + Sync + 'static,
    TContext: Send + Sync + 'static,
{
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            options: IndexMap::new(),
            subcommands: IndexMap::new(),
            builder: None,
            command_action: None,
        }
    }

    pub fn option<TValue: Clone + Send + Sync + 'static>(mut self, option: &CommandOption<TValue>) -> Self {
        if self.options.contains_key(option.name) {
            panic!("Option {} is already registered for {}", option.name, self.name);
        }
        self.options.insert(option.name, option.erased());
        self
    }

    pub fn build<F>(mut self, builder: F) -> Self
    where
        F: Fn(&ParsedCommandInput) -> CommandBuildResult<TOwnInvocation> + Send + Sync + 'static,
    {
        self.builder = Some(Arc::new(builder));
        self
    }

    pub fn action<F>(mut self, action: F) -> Self
    where
        F: Fn(TOwnInvocation, &TContext) -> futures::future::BoxFuture<'_, pi_js::Result<()>> + Send + Sync + 'static,
    {
        self.command_action = Some(Arc::new(action));
        self
    }

    pub fn subcommand<TSubOwn, TSubContext, FMapInv, FMapCtx>(
        mut self,
        command: Command<TSubOwn, TSubContext>,
        map_invocation: FMapInv,
        map_context: FMapCtx,
    ) -> Self
    where
        TSubOwn: Clone + Send + Sync + 'static,
        TSubContext: Send + Sync + 'static,
        FMapInv: Fn(TSubOwn) -> TOwnInvocation + Send + Sync + 'static,
        FMapCtx: Fn(&TContext) -> &TSubContext + Send + Sync + 'static,
    {
        let sub_name = command.name.clone();
        if self.subcommands.contains_key(&sub_name) {
            panic!("Command {sub_name} is already registered");
        }
        self.subcommands.insert(
            sub_name,
            Arc::new(MappedSubcommand {
                inner: command,
                map_invocation,
                map_context,
                _marker: std::marker::PhantomData,
            }),
        );
        self
    }

    pub fn parse(&self, argv: &[String]) -> CommandParseResult<TOwnInvocation> {
        if let Some((sub, rest)) = self.select(argv) {
            return sub.parse(rest);
        }
        self.parse_own(argv)
    }

    pub fn parse_strs(&self, argv: &[&str]) -> CommandParseResult<TOwnInvocation> {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        self.parse(&owned)
    }

    pub async fn execute(
        &self,
        argv: &[String],
        context: &TContext,
    ) -> pi_js::Result<CommandExecutionResult<TOwnInvocation>> {
        if let Some((sub, rest)) = self.select(argv) {
            return sub.execute(rest, context).await;
        }
        let parsed = self.parse_own(argv);
        let command = match parsed {
            CommandParseResult::Ok { command } => command,
            CommandParseResult::Err { errors } => return Ok(CommandParseResult::Err { errors }),
        };
        let Some(ref action) = self.command_action else {
            return Err(pi_js::Error::msg(format!(
                "Command {} does not define an action",
                self.name
            )));
        };
        action(command.clone(), context).await?;
        Ok(CommandParseResult::Ok { command })
    }

    pub async fn execute_strs(
        &self,
        argv: &[&str],
        context: &TContext,
    ) -> pi_js::Result<CommandExecutionResult<TOwnInvocation>> {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        self.execute(&owned, context).await
    }

    fn select<'a>(
        &self,
        argv: &'a [String],
    ) -> Option<(&Arc<dyn RegisteredSubcommand<TOwnInvocation, TContext>>, &'a [String])> {
        let candidate = argv.first()?;
        let sub = self.subcommands.get(candidate.as_str())?;
        Some((sub, &argv[1..]))
    }

    fn parse_own(&self, argv: &[String]) -> CommandParseResult<TOwnInvocation> {
        let Some(ref builder) = self.builder else {
            return CommandParseResult::Err {
                errors: vec![format!("Command {} does not define a builder", self.name)],
            };
        };
        let parsed = self.parse_options(argv);
        let input = ParsedCommandInput {
            values: parsed.values,
            remaining_args: parsed.remaining_args,
        };
        let built = builder(&input);
        let mut errors = parsed.errors;
        if let CommandParseResult::Err { errors: build_errors } = &built {
            errors.extend(build_errors.iter().cloned());
        }
        if !errors.is_empty() {
            return CommandParseResult::Err { errors };
        }
        match built {
            CommandParseResult::Ok { command } => CommandParseResult::Ok { command },
            CommandParseResult::Err { errors } => CommandParseResult::Err { errors },
        }
    }

    fn parse_options(&self, argv: &[String]) -> MutableParsedCommandInput {
        let mut parsed = MutableParsedCommandInput {
            values: IndexMap::new(),
            remaining_args: Vec::new(),
            errors: Vec::new(),
        };
        let mut index = 0usize;
        while index < argv.len() {
            let argument = &argv[index];
            if argument == "--" {
                parsed.remaining_args.extend(argv[index..].iter().cloned());
                break;
            }

            let equals = argument.find('=');
            let name = match equals {
                Some(pos) => &argument[..pos],
                None => argument.as_str(),
            };
            let Some(option) = self.options.get(name) else {
                parsed.remaining_args.extend(argv[index..].iter().cloned());
                break;
            };

            let value = if option.flag {
                if equals.is_some() {
                    parsed.errors.push(format!("{name} does not take a value"));
                    index += 1;
                    continue;
                }
                String::new()
            } else {
                let mut candidate = equals.map(|pos| &argument[pos + 1..]);
                if candidate.is_none()
                    && let Some(next) = argv.get(index + 1)
                    && !next.starts_with('-')
                {
                    candidate = Some(next.as_str());
                    index += 1;
                }
                match candidate {
                    Some(v) if !v.is_empty() => v.to_string(),
                    _ => {
                        parsed.errors.push(format!("{name} requires a value"));
                        index += 1;
                        continue;
                    }
                }
            };

            let existing_len = parsed.values.get(name).map_or(0, Vec::len);
            if existing_len > 0 && !option.repeatable {
                parsed.errors.push(format!("{name} may only be specified once"));
                index += 1;
                continue;
            }
            match (option.parse_erased)(&value) {
                Ok(val) => {
                    parsed.values.entry(name.to_string()).or_default().push(val);
                }
                Err(error) => {
                    parsed.errors.push(error);
                }
            }
            index += 1;
        }
        parsed
    }
}
