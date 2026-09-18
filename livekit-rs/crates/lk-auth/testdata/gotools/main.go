// Command gotools mints and verifies LiveKit access tokens with the Go
// implementation, so the Rust port can be checked against the real thing
// rather than against a second reading of the spec.
//
// Run it from the repository root, where the Go module lives:
//
//	go run ./livekit-rs/crates/lk-auth/testdata/gotools mint   > fixtures.json
//	go run ./livekit-rs/crates/lk-auth/testdata/gotools verify <token>
//
// `mint` writes the fixtures checked in beside this file. They are signed with
// a throwaway secret and are deliberately long-lived, because a fixture that
// expires turns into a CI failure with no code change behind it.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/livekit/protocol/auth"
	"github.com/livekit/protocol/livekit"
)

const (
	apiKey    = "devkey"
	apiSecret = "secret-that-is-at-least-32-characters"
	validFor  = 100 * 365 * 24 * time.Hour
)

type fixture struct {
	Name  string `json:"name"`
	Token string `json:"token"`
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: gotools mint|verify [token]")
		os.Exit(2)
	}

	switch os.Args[1] {
	case "mint":
		mint()
	case "verify":
		if len(os.Args) < 3 {
			fmt.Fprintln(os.Stderr, "usage: gotools verify <token>")
			os.Exit(2)
		}
		verify(os.Args[2])
	default:
		fmt.Fprintf(os.Stderr, "unknown command %q\n", os.Args[1])
		os.Exit(2)
	}
}

func mint() {
	fixtures := []fixture{
		{Name: "join", Token: must(joinToken())},
		{Name: "full_grants", Token: must(fullGrantsToken())},
		{Name: "room_config", Token: must(roomConfigToken())},
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(fixtures); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func joinToken() (string, error) {
	return auth.NewAccessToken(apiKey, apiSecret).
		SetIdentity("alice").
		SetName("Alice").
		SetValidFor(validFor).
		SetVideoGrant(&auth.VideoGrant{RoomJoin: true, Room: "my-room"}).
		ToJWT()
}

func fullGrantsToken() (string, error) {
	video := &auth.VideoGrant{
		RoomCreate:      true,
		RoomList:        true,
		RoomRecord:      true,
		RoomAdmin:       true,
		RoomJoin:        true,
		Room:            "my-room",
		IngressAdmin:    true,
		Hidden:          true,
		Recorder:        true,
		Agent:           true,
		DestinationRoom: "other-room",
	}
	video.SetCanPublish(false)
	video.SetCanSubscribe(true)
	video.SetCanPublishData(true)
	video.SetCanUpdateOwnMetadata(true)
	video.SetCanSubscribeMetrics(true)
	video.SetCanManageAgentSession(true)
	video.SetCanPublishSources([]livekit.TrackSource{
		livekit.TrackSource_CAMERA,
		livekit.TrackSource_SCREEN_SHARE,
	})

	return auth.NewAccessToken(apiKey, apiSecret).
		SetIdentity("bob").
		SetName("Bob").
		SetKind(livekit.ParticipantInfo_AGENT).
		SetKindDetail(livekit.ParticipantInfo_FORWARDED).
		SetValidFor(validFor).
		SetVideoGrant(video).
		SetSIPGrant(&auth.SIPGrant{Admin: true, Call: true}).
		SetAgentGrant(&auth.AgentGrant{Admin: true, SimulationAdmin: true, DatabaseAdmin: true, DispatchAdmin: true}).
		SetInferenceGrant(&auth.InferenceGrant{Perform: true}).
		SetObservabilityGrant(&auth.ObservabilityGrant{Write: true}).
		SetMetadata("some-metadata").
		SetAttributes(map[string]string{"seat": "12A", "tier": "gold"}).
		SetSha256("abc123").
		SetRoomPreset("preset-1").
		ToJWT()
}

func roomConfigToken() (string, error) {
	return auth.NewAccessToken(apiKey, apiSecret).
		SetIdentity("carol").
		SetValidFor(validFor).
		SetVideoGrant(&auth.VideoGrant{RoomJoin: true, Room: "configured-room"}).
		SetRoomConfig(&livekit.RoomConfiguration{
			Name:             "configured-room",
			EmptyTimeout:     120,
			DepartureTimeout: 30,
			MaxParticipants:  4,
			Agents: []*livekit.RoomAgentDispatch{
				{AgentName: "assistant", Metadata: "{}"},
			},
		}).
		ToJWT()
}

// verify checks a token minted elsewhere (the Rust port) against the Go
// verifier and prints the grants it read.
func verify(token string) {
	v, err := auth.ParseAPIToken(token)
	if err != nil {
		fmt.Fprintln(os.Stderr, "parse:", err)
		os.Exit(1)
	}
	if v.APIKey() != apiKey {
		fmt.Fprintf(os.Stderr, "unexpected api key %q\n", v.APIKey())
		os.Exit(1)
	}
	_, grants, err := v.Verify(apiSecret)
	if err != nil {
		fmt.Fprintln(os.Stderr, "verify:", err)
		os.Exit(1)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(grants); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func must(token string, err error) string {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	return token
}
