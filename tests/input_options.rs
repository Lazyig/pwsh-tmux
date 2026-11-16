//! Command input compatibility options through the real client/server protocol.
mod common;
use common::*;

#[test]
fn default_client_command_is_run_by_an_empty_client_request() {
    let ctx = Ctx::new("input-default");
    ctx.ok(&["new-session", "-d", "-s", "input"]);
    ctx.ok(&["set", "-s", "default-client-command", "display-message -p DEFAULT-CLIENT"]);
    let output = ctx.command().output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "DEFAULT-CLIENT");
    ctx.ok(&["set", "-s", "default-client-command", "set -g @literal-semicolon ';'"]);
    assert!(ctx.command().output().unwrap().status.success());
    assert_eq!(ctx.fmt("input", "#{@literal-semicolon}"), ";");
}

#[test]
fn command_aliases_expand_multiple_commands_and_preserve_arguments() {
    let ctx = Ctx::new("input-alias");
    ctx.ok(&["new-session", "-d", "-s", "input"]);
    ctx.ok(&["set", "-s", "command-alias[100]", "remember=set -g @before yes ; set -g"]);
    ctx.ok(&["remember", "@value", "spaces ; and quotes ' stay literal"]);
    assert_eq!(ctx.fmt("input", "#{@before}"), "yes");
    assert_eq!(ctx.fmt("input", "#{@value}"), "spaces ; and quotes ' stay literal");
    ctx.ok(&["set", "-s", "command-alias[100]", "remember=set -g @replacement yes"]);
    ctx.ok(&["remember"]);
    assert_eq!(ctx.fmt("input", "#{@replacement}"), "yes");
}
