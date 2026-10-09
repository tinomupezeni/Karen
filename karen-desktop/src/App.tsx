import { useState, useEffect } from 'react'
import { ThinkingOrb } from 'thinking-orbs'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'

type OrbState = "working" | "searching" | "solving" | "listening" | "connecting" | "weaving" | "composing" | "breathing" | "shaping";

type PipelineStage = "model-missing" | "transcribing" | "thinking" | "running-tool" | "answer" | "error" | "idle";
type PipelineEvent = { stage: PipelineStage; detail?: string };

function App() {
  const [orbState, setOrbState] = useState<OrbState>("listening")
  const [isMuted, setIsMuted] = useState(false)
  const [displayText, setDisplayText] = useState("Karen is listening...")

  useEffect(() => {
    const unlistenWake = listen('wake-karen', () => {
      // Force wake / barge-in
      setIsMuted(false);
      setOrbState("listening");
      setDisplayText("Hi Tino, I'm listening...");
    });

    const unlistenToggle = listen('toggle-mic', () => {
      setIsMuted((prev) => {
        const nextMuted = !prev;
        if (nextMuted) {
          setOrbState("breathing");
          setDisplayText("Microphone Muted");
        } else {
          setOrbState("listening");
          setDisplayText("Microphone Active");
        }
        return nextMuted;
      });
    });

    const unlistenPipeline = listen<PipelineEvent>('karen-pipeline', ({ payload }) => {
      switch (payload.stage) {
        case "model-missing":
          setOrbState("working");
          setDisplayText(payload.detail ?? "Setting up speech recognition...");
          break;
        case "transcribing":
          setOrbState("working");
          setDisplayText("Transcribing...");
          break;
        case "thinking":
          setOrbState("solving");
          setDisplayText(payload.detail ? `"${payload.detail}"` : "Thinking...");
          break;
        case "running-tool":
          setOrbState("connecting");
          setDisplayText(payload.detail ?? "Working on it...");
          break;
        case "answer":
          setOrbState("breathing");
          setDisplayText(payload.detail ?? "Done");
          break;
        case "error":
          setOrbState("breathing");
          setDisplayText(payload.detail ? `Error: ${payload.detail}` : "Something went wrong");
          break;
        case "idle":
          setOrbState("breathing");
          setDisplayText(payload.detail ?? "Ready");
          break;
      }
    });

    return () => {
      unlistenWake.then(f => f());
      unlistenToggle.then(f => f());
      unlistenPipeline.then(f => f());
    };
  }, []);

  return (
    <div 
      style={{
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'center',
        alignItems: 'center',
        width: '100vw',
        height: '100vh',
        background: 'transparent',
        cursor: 'pointer',
        padding: '20px',
        gap: '16px'
      }}
    >
      {/* Frosted Glass Container - click to toggle listening */}
      <div
        onClick={() => invoke('toggle_mic_command')}
        style={{
          display: 'flex',
          justifyContent: 'center',
          alignItems: 'center',
          width: '150px',
          height: '150px',
          background: isMuted ? 'rgba(50, 10, 10, 0.4)' : 'rgba(20, 20, 20, 0.4)',
          backdropFilter: 'blur(12px)',
          WebkitBackdropFilter: 'blur(12px)',
          borderRadius: '50%',
          boxShadow: '0 8px 32px 0 rgba(0, 0, 0, 0.37)',
          border: isMuted ? '1px solid rgba(255, 50, 50, 0.2)' : '1px solid rgba(255, 255, 255, 0.1)',
          transition: 'all 0.3s ease'
        }}>
        {/* Scaled up Orb */}
        <div style={{ transform: 'scale(1.8)', opacity: isMuted ? 0.4 : 1.0, transition: 'opacity 0.3s ease' }}>
          <ThinkingOrb 
            state={orbState} 
            size={64} 
            theme="dark" 
          />
        </div>
      </div>

      {/* Subtitle Text */}
      <div style={{
        color: isMuted ? '#ff8888' : 'white',
        fontFamily: 'system-ui, -apple-system, sans-serif',
        fontSize: '14px',
        fontWeight: 500,
        background: 'rgba(20, 20, 20, 0.6)',
        backdropFilter: 'blur(8px)',
        padding: '8px 16px',
        borderRadius: '20px',
        border: isMuted ? '1px solid rgba(255, 50, 50, 0.2)' : '1px solid rgba(255, 255, 255, 0.1)',
        boxShadow: '0 4px 12px 0 rgba(0, 0, 0, 0.2)',
        textAlign: 'center',
        transition: 'all 0.3s ease'
      }}>
        {displayText}
      </div>
    </div>
  )
}

export default App
