FUNCTION_BLOCK LevelControllerFb2
VAR_INPUT
    LevelPct : REAL;
    StartCmd : BOOL;
    StopCmd : BOOL;
END_VAR
VAR_OUTPUT
    FillCmd : BOOL;
    DrainCmd : BOOL;
    Mode : E_Mode;
END_VAR
VAR
    target : REAL := 70.0;
    hysteresis : REAL := 5.0;
END_VAR

IF StopCmd THEN
    Mode := E_Mode#Stopped;
ELSIF StartCmd THEN
    IF LevelPct < target - hysteresis THEN
        Mode := E_Mode#Filling;
    ELSIF LevelPct > target + hysteresis THEN
        Mode := E_Mode#Draining;
    ELSE
        Mode := E_Mode#Stopped;
    END_IF;
END_IF;

FillCmd := Mode = E_Mode#Filling;
DrainCmd := Mode = E_Mode#Draining;
END_FUNCTION_BLOCK
