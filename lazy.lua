return {
    "ReeceStevens/vim-reviewer",
    build = function(plugin)
        local obj_dir = plugin.dir .. "/target/release/"
        local lua_dir = plugin.dir .. "/lua/"

        -- Build the release binary
        vim.fn.system({ "cargo", "build", "--release", "--manifest-path", plugin.dir .. "/Cargo.toml" })

        -- Determine source and destination based on platform
        local sysname = vim.loop.os_uname().sysname
        local src, dst
        if sysname == "Linux" then
            src = obj_dir .. "libvim_reviewer.so"
            dst = lua_dir .. "vim_reviewer.so"
        elseif sysname == "Darwin" then
            src = obj_dir .. "libvim_reviewer.dylib"
            dst = lua_dir .. "vim_reviewer.so"
        elseif sysname == "Windows_NT" then
            src = obj_dir .. "vim_reviewer.dll"
            dst = lua_dir .. "vim_reviewer.dll"
        else
            error("Unsupported platform: " .. sysname)
        end

        vim.loop.fs_copyfile(src, dst)
    end,
}
