//go:build a8faults

package app

import (
	"fmt"
	"os"
	"path/filepath"

	wasmkeeper "github.com/CosmWasm/wasmd/x/wasm/keeper"
	servertypes "github.com/cosmos/cosmos-sdk/server/types"
	"github.com/productscience/inference/app/a8faults"
)

// Only the explicitly tagged test binary can load this file. It is read once
// before keeper construction, never per query. All validators must use identical
// plans; block-height windows make activation/recovery consensus deterministic.
func appendA8QueryFaultOptions(options []wasmkeeper.Option, appOpts servertypes.AppOptions) []wasmkeeper.Option {
	home, _ := appOpts.Get("home").(string)
	if home == "" {
		panic("a8faults build requires explicit node home")
	}
	file, err := os.Open(filepath.Join(home, "config", "a8-query-faults.json"))
	if os.IsNotExist(err) {
		return options
	}
	if err != nil {
		panic(fmt.Errorf("a8faults plan: %w", err))
	}
	defer file.Close()
	plan, digest, err := a8faults.Load(file)
	if err != nil {
		panic(fmt.Errorf("a8faults plan: %w", err))
	}
	fmt.Fprintf(os.Stderr, "A8 TEST-ONLY QUERY FAULTS plan_sha256=%s rules=%d\n", digest, len(plan.Rules))
	return append(options, wasmkeeper.WithQueryHandlerDecorator(func(next wasmkeeper.WasmVMQueryHandler) wasmkeeper.WasmVMQueryHandler {
		return a8faults.Decorate(plan, next)
	}))
}
