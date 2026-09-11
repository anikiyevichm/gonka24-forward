//go:build !a8faults

package app

import (
	wasmkeeper "github.com/CosmWasm/wasmd/x/wasm/keeper"
	servertypes "github.com/cosmos/cosmos-sdk/server/types"
)

// Normal binaries never read fault plans or install a fault decorator.
func appendA8QueryFaultOptions(options []wasmkeeper.Option, _ servertypes.AppOptions) []wasmkeeper.Option {
	return options
}
