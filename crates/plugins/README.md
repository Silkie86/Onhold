# Tenant plugins

One crate per tenant slug, implementing `plugin-sdk`'s `Plugin` trait and
activated only for workspaces whose `plugin_slug` matches. The `_template`
crate and a worked example arrive in M8. Each plugin carries a `README.md`
naming the client and the ask it exists for.
