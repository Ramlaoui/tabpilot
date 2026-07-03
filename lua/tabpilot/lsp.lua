local M = {}

local providers = {
	codex = true,
	claude = true,
}

local config = {}
local auto_start_group = vim.api.nvim_create_augroup("TabpilotAutoStart", {
	clear = false,
})
local inline_trigger_group = vim.api.nvim_create_augroup("TabpilotInlineCompletion", {
	clear = false,
})

local function plugin_root()
	local source = debug.getinfo(1, "S").source
	if source:sub(1, 1) == "@" then
		source = source:sub(2)
	end

	return vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(source)))
end

local function binary_path()
	return plugin_root() .. "/target/debug/tabpilot-lsp"
end

local function command()
	return config.cmd or { binary_path() }
end

local function provider()
	local configured = config.provider or vim.g.tabpilot_provider or vim.env.TABPILOT_PROVIDER or "codex"
	return tostring(configured):lower()
end

local function validate_provider(value)
	local next_provider = tostring(value or ""):lower()
	if providers[next_provider] then
		return next_provider
	end

	return nil
end

local function command_env(opts)
	opts = opts or {}
	local env = vim.tbl_extend("force", config.cmd_env or {}, opts.cmd_env or {})
	env.TABPILOT_PROVIDER = opts.provider or provider()
	return env
end

local function auto_start_enabled()
	return config.auto_start ~= false
end

local function should_start_for_buffer(bufnr)
	if not vim.api.nvim_buf_is_loaded(bufnr) then
		return false
	end

	if vim.bo[bufnr].buftype ~= "" then
		return false
	end

	return true
end

local function normal_buffers_from_clients()
	local buffers = {}

	for _, client in ipairs(vim.lsp.get_clients({ name = "tabpilot" })) do
		for bufnr, attached in pairs(client.attached_buffers or {}) do
			if attached and should_start_for_buffer(bufnr) then
				buffers[bufnr] = true
			end
		end
	end

	if next(buffers) == nil then
		local bufnr = vim.api.nvim_get_current_buf()
		if should_start_for_buffer(bufnr) then
			buffers[bufnr] = true
		end
	end

	return buffers
end

local function stop_clients()
	for _, client in ipairs(vim.lsp.get_clients({ name = "tabpilot" })) do
		vim.lsp.stop_client(client.id, true)
	end
end

local function request_inline_completion(bufnr)
	local ok, capability = pcall(require, "vim.lsp._capability")
	if not ok then
		return
	end

	local inline_completion = capability.all and capability.all.inline_completion
	local completor = inline_completion and inline_completion.active[bufnr]
	if completor and completor.automatic_request then
		completor:automatic_request()
	end
end

local function enable_insert_change_trigger(bufnr)
	vim.api.nvim_clear_autocmds({
		group = inline_trigger_group,
		buffer = bufnr,
	})
	vim.api.nvim_create_autocmd("TextChangedI", {
		group = inline_trigger_group,
		buffer = bufnr,
		callback = function()
			request_inline_completion(bufnr)
		end,
		desc = "Request Tabpilot inline completion after insert-mode text changes",
	})
end

local function start_buffer(bufnr)
	if not auto_start_enabled() or not should_start_for_buffer(bufnr) then
		return nil
	end

	return M.start({ bufnr = bufnr })
end

local function enable_auto_start()
	vim.api.nvim_clear_autocmds({
		group = auto_start_group,
	})
	vim.api.nvim_create_autocmd({ "BufEnter", "BufWinEnter" }, {
		group = auto_start_group,
		callback = function(args)
			start_buffer(args.buf)
		end,
		desc = "Start Tabpilot for normal buffers",
	})

	vim.schedule(function()
		start_buffer(vim.api.nvim_get_current_buf())
	end)
end

function M.setup(opts)
	config = opts or {}
	config.provider = validate_provider(config.provider) or provider()

	vim.api.nvim_create_user_command("TabpilotStart", function()
		M.start()
	end, {})

	vim.api.nvim_create_user_command("TabpilotRestart", function()
		M.restart()
	end, {})

	vim.api.nvim_create_user_command("TabpilotProvider", function(command_opts)
		if command_opts.args == "" then
			vim.notify(
				("provider: %s"):format(provider()),
				vim.log.levels.INFO,
				{ title = "Tabpilot" }
			)
			return
		end

		M.set_provider(command_opts.args)
	end, {
		nargs = "?",
		complete = function()
			return vim.tbl_keys(providers)
		end,
	})

	vim.api.nvim_create_user_command("TabpilotStatus", function()
		M.status()
	end, {})

	if auto_start_enabled() then
		enable_auto_start()
	end
end

function M.start(opts)
	opts = opts or {}
	local cmd = opts.cmd or command()
	local bufnr = opts.bufnr or vim.api.nvim_get_current_buf()

	if not should_start_for_buffer(bufnr) then
		return nil
	end

	local client_id = vim.lsp.start({
		name = "tabpilot",
		cmd = cmd,
		cmd_env = command_env(opts),
		root_dir = opts.root_dir or vim.uv.cwd(),
	}, {
		bufnr = bufnr,
	})

	if not client_id then
		return nil
	end

	vim.lsp.inline_completion.enable(true, {
		bufnr = bufnr,
	})
	enable_insert_change_trigger(bufnr)

	vim.keymap.set("i", "<M-l>", function()
		if vim.lsp.inline_completion.get({ bufnr = bufnr }) then
			return ""
		end
		return "<M-l>"
	end, {
		buffer = bufnr,
		expr = true,
		desc = "Accept Tabpilot inline completion",
	})

	return client_id
end

function M.restart(opts)
	opts = opts or {}
	local buffers = normal_buffers_from_clients()
	stop_clients()

	vim.defer_fn(function()
		for bufnr, _ in pairs(buffers) do
			M.start(vim.tbl_extend("force", opts, { bufnr = bufnr }))
		end
	end, opts.delay_ms or 100)
end

function M.provider()
	return provider()
end

function M.set_provider(next_provider)
	next_provider = validate_provider(next_provider)
	if not next_provider then
		vim.notify(
			"provider must be one of: codex, claude",
			vim.log.levels.ERROR,
			{ title = "Tabpilot" }
		)
		return nil
	end

	if config.provider == next_provider then
		vim.notify(
			("provider already set to %s"):format(next_provider),
			vim.log.levels.INFO,
			{ title = "Tabpilot" }
		)
		return next_provider
	end

	config.provider = next_provider
	vim.g.tabpilot_provider = next_provider
	vim.notify(
		("provider set to %s; restarting"):format(next_provider),
		vim.log.levels.INFO,
		{ title = "Tabpilot" }
	)
	M.restart({ provider = next_provider })
	return next_provider
end

function M.status()
	local cmd = command()
	local binary = cmd[1]
	local executable = vim.fn.executable(binary) == 1 and "yes" or "no"
	local clients = vim.lsp.get_clients({ name = "tabpilot" })

	vim.notify(
		("tabpilot-lsp: %s\nexecutable: %s\nprovider: %s\nclients: %d"):format(
			binary,
			executable,
			provider(),
			#clients
		),
		vim.log.levels.INFO,
		{ title = "Tabpilot" }
	)
end

return M
