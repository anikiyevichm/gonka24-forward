package app

import (
	"bytes"
	"errors"
	"testing"

	wasmkeeper "github.com/CosmWasm/wasmd/x/wasm/keeper"
	wasmvmtypes "github.com/CosmWasm/wasmvm/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
	"github.com/cosmos/gogoproto/proto"
	"github.com/productscience/inference/x/inference/types"
	"github.com/stretchr/testify/require"
)

// This fixture is deliberately test-only. It specifies the narrow matching
// rule a future A8 runtime decorator must use; it is not linked into a node.
const a8EpochSummaryPath = "/inference.inference.Query/EpochPerformanceSummaryByParticipant"
const a8CurrentEpochPath = "/inference.inference.Query/GetCurrentEpoch"
const a8ClaimRecipientsPath = "/inference.inference.Query/ListClaimRecipients"

// These are two distinct, checksum-valid Gonka account addresses from the
// pinned app source. The wrong-host fixture must be a valid other identity so
// Marketplace reaches IdentityMismatch rather than InvalidAddress.
const a8TargetHost = "gonka1y2a9p56kv044327uycmqdexl7zs82fs5ryv5le"
const a8OtherValidHost = "gonka1dkl4mah5erqggvhqkpc8j3qs5tyuetgdy552cp"

// This is deliberately equal to Marketplace's MAX_GRPC_RESPONSE_BYTES. The
// test-only native provider returns this exact value plus one byte, allowing
// the Rust adapter to prove its own bound before protobuf decode.
const a8MarketplaceMaxGrpcResponseBytes = 32 * 1024

type a8FaultKind string

const (
	a8HandlerError        a8FaultKind = "handler_error"
	a8Unsupported         a8FaultKind = "unsupported_request"
	a8BadProto            a8FaultKind = "malformed_protobuf"
	a8Oversized           a8FaultKind = "oversized_response"
	a8MissingNested       a8FaultKind = "missing_nested_summary"
	a8WrongHost           a8FaultKind = "wrong_host"
	a8WrongEpoch          a8FaultKind = "wrong_epoch"
	a8InvalidParticipant a8FaultKind = "invalid_participant_address"
	a8InvalidJSON         a8FaultKind = "invalid_response_envelope"
)

type a8FaultLayer string

const (
	a8ContractResultLayer a8FaultLayer = "contract_result"
	a8SystemResultLayer   a8FaultLayer = "system_result"
	a8RawResponseLayer    a8FaultLayer = "raw_response"
)

// a8FaultDeliveryLayer prevents a test plan from presenting an ordinary Go
// error as a typed SystemError. Invalid JSON must be injected at the FFI JSON
// envelope, not returned by the keeper.
func a8FaultDeliveryLayer(kind a8FaultKind) a8FaultLayer {
	switch kind {
	case a8HandlerError:
		return a8ContractResultLayer
	case a8Unsupported:
		return a8SystemResultLayer
	case a8BadProto, a8Oversized, a8MissingNested, a8WrongHost, a8WrongEpoch, a8InvalidParticipant:
		return a8RawResponseLayer
	case a8InvalidJSON:
		return a8SystemResultLayer
	default:
		panic("unknown A8 fault kind")
	}
}

type a8SummaryFaultSelector struct {
	Enabled bool
	Host    string
	Epoch   uint64
	Kind    a8FaultKind
}

// a8RouteFaultScope is the selector for R3. The current-epoch request has no
// Host/E fields, so its equivalent isolation key is the exact calling Deal
// address plus the route. Summary and routing requests additionally decode and
// check the immutable Host (and Host/E for summary) rather than trusting a
// caller-supplied string.
type a8RouteFaultScope struct {
	Enabled bool
	Route   string
	Deal    string
	Host    string
	Epoch   uint64
}

func (s a8RouteFaultScope) matches(request *wasmvmtypes.GrpcQuery, caller sdk.AccAddress) bool {
	if !s.Enabled || request == nil || request.Path != s.Route || caller.String() != s.Deal {
		return false
	}
	switch s.Route {
	case a8CurrentEpochPath:
		var decoded types.QueryGetCurrentEpochRequest
		return proto.Unmarshal(request.Data, &decoded) == nil
	case a8ClaimRecipientsPath:
		var decoded types.QueryListClaimRecipientsRequest
		return proto.Unmarshal(request.Data, &decoded) == nil && decoded.Participant == s.Host
	case a8EpochSummaryPath:
		var decoded types.QueryEpochPerformanceSummaryByParticipantRequest
		return proto.Unmarshal(request.Data, &decoded) == nil &&
			decoded.ParticipantId == s.Host && decoded.EpochIndex == s.Epoch
	default:
		return false
	}
}

// matches is fail-closed: a disabled selector, another route, malformed
// request, another host, or another epoch is always delegated unchanged.
func (s a8SummaryFaultSelector) matches(request *wasmvmtypes.GrpcQuery) bool {
	if !s.Enabled || request == nil || request.Path != a8EpochSummaryPath {
		return false
	}
	var decoded types.QueryEpochPerformanceSummaryByParticipantRequest
	if err := proto.Unmarshal(request.Data, &decoded); err != nil {
		return false
	}
	return decoded.ParticipantId == s.Host && decoded.EpochIndex == s.Epoch
}

// a8SummaryFaultDecorator is the concrete test-only wrapper proposed for an
// instrumented A8 node. It operates below QueryPlugins.HandleQuery, therefore
// it can honestly return raw protobuf bytes while delegating all nonmatching
// requests to the real handler unchanged. It is intentionally defined in a
// _test.go file and is not available to a production node binary.
func a8SummaryFaultDecorator(
	selector a8SummaryFaultSelector,
	next wasmkeeper.WasmVMQueryHandler,
) wasmkeeper.WasmVMQueryHandler {
	return wasmkeeper.WasmVMQueryHandlerFn(func(
		ctx sdk.Context,
		caller sdk.AccAddress,
		request wasmvmtypes.QueryRequest,
	) ([]byte, error) {
		if !selector.matches(request.Grpc) {
			return next.HandleQuery(ctx, caller, request)
		}

		switch selector.Kind {
		case a8HandlerError:
			return nil, errors.New("a8 test-only summary handler failure")
		case a8Unsupported:
			return nil, wasmvmtypes.UnsupportedRequest{Kind: "a8 test-only summary fault"}
		case a8BadProto:
			return []byte{0xff}, nil
		case a8Oversized:
			return bytes.Repeat([]byte{0}, a8MarketplaceMaxGrpcResponseBytes+1), nil
		case a8MissingNested:
			// Gonka's generated response stores this message as a non-pointer
			// value and always marshals field 1, even when it is empty (0a00).
			// Rust prost reads 0a00 as Some(default), not None. An empty
			// protobuf response is the valid wire representation with field 1
			// genuinely absent.
			return []byte{}, nil
		case a8WrongHost:
			return proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantResponse{
				EpochPerformanceSummary: types.EpochPerformanceSummary{
					EpochIndex: selector.Epoch, ParticipantId: a8OtherValidHost,
				},
			})
		case a8WrongEpoch:
			return proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantResponse{
				EpochPerformanceSummary: types.EpochPerformanceSummary{
					EpochIndex: selector.Epoch + 1, ParticipantId: selector.Host,
				},
			})
		case a8InvalidParticipant:
			return proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantResponse{
				EpochPerformanceSummary: types.EpochPerformanceSummary{
					EpochIndex: selector.Epoch, ParticipantId: "not-a-gonka-address",
				},
			})
		default:
			panic("unknown A8 fault kind")
		}
	})
}

func TestA8SummaryFaultSelectorIsNarrowAndDeterministic(t *testing.T) {
	selector := a8SummaryFaultSelector{
		Enabled: true,
		Host:    a8TargetHost,
		Epoch:   42,
		Kind:    a8HandlerError,
	}

	request := func(path, host string, epoch uint64) *wasmvmtypes.GrpcQuery {
		data, err := proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantRequest{
			ParticipantId: host,
			EpochIndex:     epoch,
		})
		require.NoError(t, err)
		return &wasmvmtypes.GrpcQuery{Path: path, Data: data}
	}

	require.True(t, selector.matches(request(a8EpochSummaryPath, a8TargetHost, 42)))
	require.False(t, selector.matches(request("/inference.inference.Query/GetCurrentEpoch", a8TargetHost, 42)))
	require.False(t, selector.matches(request(a8EpochSummaryPath, a8OtherValidHost, 42)))
	require.False(t, selector.matches(request(a8EpochSummaryPath, a8TargetHost, 43)))
	require.False(t, selector.matches(&wasmvmtypes.GrpcQuery{Path: a8EpochSummaryPath, Data: []byte{0xff}}))

	selector.Enabled = false
	require.False(t, selector.matches(request(a8EpochSummaryPath, a8TargetHost, 42)))
}

func TestA8RouteFaultScopeUsesDealForCurrentEpochAndDecodedKeysElsewhere(t *testing.T) {
	const deal = "gonka1a8deal"
	caller := sdk.AccAddress([]byte(deal))
	otherCaller := sdk.AccAddress([]byte("gonka1anotherdeal"))
	marshal := func(message proto.Message) []byte {
		data, err := proto.Marshal(message)
		require.NoError(t, err)
		return data
	}

	current := a8RouteFaultScope{Enabled: true, Route: a8CurrentEpochPath, Deal: caller.String()}
	require.True(t, current.matches(&wasmvmtypes.GrpcQuery{
		Path: a8CurrentEpochPath, Data: marshal(&types.QueryGetCurrentEpochRequest{}),
	}, caller))
	require.False(t, current.matches(&wasmvmtypes.GrpcQuery{
		Path: a8CurrentEpochPath, Data: marshal(&types.QueryGetCurrentEpochRequest{}),
	}, otherCaller))
	require.False(t, current.matches(&wasmvmtypes.GrpcQuery{
		Path: a8CurrentEpochPath, Data: []byte{0xff},
	}, caller))

	routing := a8RouteFaultScope{Enabled: true, Route: a8ClaimRecipientsPath, Deal: caller.String(), Host: "gonka1host"}
	require.True(t, routing.matches(&wasmvmtypes.GrpcQuery{
		Path: a8ClaimRecipientsPath,
		Data: marshal(&types.QueryListClaimRecipientsRequest{Participant: "gonka1host"}),
	}, caller))
	require.False(t, routing.matches(&wasmvmtypes.GrpcQuery{
		Path: a8ClaimRecipientsPath,
		Data: marshal(&types.QueryListClaimRecipientsRequest{Participant: "gonka1other"}),
	}, caller))
}

func TestA8FaultKindsKeepTheirActualWasmBoundary(t *testing.T) {
	tests := map[a8FaultKind]a8FaultLayer{
		a8HandlerError:        a8ContractResultLayer,
		a8Unsupported:         a8SystemResultLayer,
		a8BadProto:            a8RawResponseLayer,
		a8Oversized:           a8RawResponseLayer,
		a8MissingNested:       a8RawResponseLayer,
		a8WrongHost:           a8RawResponseLayer,
		a8WrongEpoch:          a8RawResponseLayer,
		a8InvalidParticipant: a8RawResponseLayer,
		a8InvalidJSON:         a8SystemResultLayer,
	}
	for kind, want := range tests {
		require.Equal(t, want, a8FaultDeliveryLayer(kind), string(kind))
	}
}

func TestA8SummaryFaultDecoratorDelegatesUnlessExactHostEpochIsEnabled(t *testing.T) {
	var delegated int
	next := wasmkeeper.WasmVMQueryHandlerFn(func(
		_ sdk.Context,
		_ sdk.AccAddress,
		_ wasmvmtypes.QueryRequest,
	) ([]byte, error) {
		delegated++
		return []byte("healthy"), nil
	})
	selector := a8SummaryFaultSelector{
		Enabled: true, Host: a8TargetHost, Epoch: 42, Kind: a8BadProto,
	}
	decorated := a8SummaryFaultDecorator(selector, next)
	request := func(path, host string, epoch uint64) wasmvmtypes.QueryRequest {
		data, err := proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantRequest{
			ParticipantId: host, EpochIndex: epoch,
		})
		require.NoError(t, err)
		return wasmvmtypes.QueryRequest{Grpc: &wasmvmtypes.GrpcQuery{Path: path, Data: data}}
	}

	for _, query := range []wasmvmtypes.QueryRequest{
		request("/inference.inference.Query/GetCurrentEpoch", a8TargetHost, 42),
		request(a8EpochSummaryPath, a8OtherValidHost, 42),
		request(a8EpochSummaryPath, a8TargetHost, 43),
		{Grpc: &wasmvmtypes.GrpcQuery{Path: a8EpochSummaryPath, Data: []byte{0xff}}},
	} {
		got, err := decorated.HandleQuery(sdk.Context{}, nil, query)
		require.NoError(t, err)
		require.Equal(t, []byte("healthy"), got)
	}
	require.Equal(t, 4, delegated)

	selector.Enabled = false
	disabled := a8SummaryFaultDecorator(selector, next)
	got, err := disabled.HandleQuery(sdk.Context{}, nil, request(a8EpochSummaryPath, a8TargetHost, 42))
	require.NoError(t, err)
	require.Equal(t, []byte("healthy"), got)
	require.Equal(t, 5, delegated)
}

func TestA8SummaryFaultDecoratorProducesOnlyItsDeclaredLayer(t *testing.T) {
	next := wasmkeeper.WasmVMQueryHandlerFn(func(
		_ sdk.Context,
		_ sdk.AccAddress,
		_ wasmvmtypes.QueryRequest,
	) ([]byte, error) {
		return []byte("unexpected delegate"), nil
	})
	data, err := proto.Marshal(&types.QueryEpochPerformanceSummaryByParticipantRequest{
		ParticipantId: a8TargetHost, EpochIndex: 42,
	})
	require.NoError(t, err)
	request := wasmvmtypes.QueryRequest{Grpc: &wasmvmtypes.GrpcQuery{Path: a8EpochSummaryPath, Data: data}}

	for _, kind := range []a8FaultKind{
		a8HandlerError, a8Unsupported, a8BadProto, a8Oversized,
		a8MissingNested, a8WrongHost, a8WrongEpoch, a8InvalidParticipant,
	} {
		t.Run(string(kind), func(t *testing.T) {
			provider := a8SummaryFaultDecorator(a8SummaryFaultSelector{
				Enabled: true, Host: a8TargetHost, Epoch: 42, Kind: kind,
			}, next)
			response, faultErr := provider.HandleQuery(sdk.Context{}, nil, request)
			result := wasmvmtypes.ToQuerierResult(response, faultErr)
			switch a8FaultDeliveryLayer(kind) {
			case a8ContractResultLayer:
				require.NotNil(t, result.Ok)
				require.NotEmpty(t, result.Ok.Err)
				require.Nil(t, result.Err)
			case a8SystemResultLayer:
				require.NotNil(t, result.Err)
				require.NotNil(t, result.Err.UnsupportedRequest)
			case a8RawResponseLayer:
				require.NotNil(t, result.Ok)
				require.Empty(t, result.Ok.Err)
				require.Nil(t, result.Err)
			}

			if kind == a8Oversized {
				require.Len(t, response, a8MarketplaceMaxGrpcResponseBytes+1)
			}
			if kind == a8BadProto {
				require.Equal(t, []byte{0xff}, response)
			}
			if kind == a8MissingNested {
				require.Empty(t, response, "field 1 must be absent, not a 0a00 empty nested message")
			}
			if kind == a8WrongHost {
				var decoded types.QueryEpochPerformanceSummaryByParticipantResponse
				require.NoError(t, proto.Unmarshal(response, &decoded))
				require.Equal(t, a8OtherValidHost, decoded.EpochPerformanceSummary.ParticipantId)
				require.NotEqual(t, a8TargetHost, decoded.EpochPerformanceSummary.ParticipantId)
			}
			if kind == a8InvalidParticipant {
				var decoded types.QueryEpochPerformanceSummaryByParticipantResponse
				require.NoError(t, proto.Unmarshal(response, &decoded))
				require.Equal(t, "not-a-gonka-address", decoded.EpochPerformanceSummary.ParticipantId)
			}
		})
	}
}
