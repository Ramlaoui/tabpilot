local M = {}

function M.setup(opts)
	return require("tabpilot.lsp").setup(opts or {})
end

function M.start(opts)
	return require("tabpilot.lsp").start(opts or {})
end

function M.status()
	return require("tabpilot.lsp").status()
end

function M.provider()
	return require("tabpilot.lsp").provider()
end

function M.set_provider(provider)
	return require("tabpilot.lsp").set_provider(provider)
end

function M.restart(opts)
	return require("tabpilot.lsp").restart(opts or {})
end

return M
