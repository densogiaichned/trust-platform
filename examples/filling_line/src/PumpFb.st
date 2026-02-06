FUNCTION_BLOCK PumpFb IMPLEMENTS IPump
VAR
    running : BOOL;
    speed_value : INT;
END_VAR

METHOD PUBLIC Start : BOOL
    running := TRUE;
    Start := running;
END_METHOD

METHOD PUBLIC Stop : BOOL
    running := FALSE;
    Stop := running;
END_METHOD

PUBLIC PROPERTY IsRunning : BOOL
    GET
        IsRunning := running;
    END_GET
    SET
    END_SET
END_PROPERTY

PUBLIC PROPERTY Speed : INT
    GET
        Speed := speed_value;
    END_GET
    SET
        speed_value := Speed;
    END_SET
END_PROPERTY
END_FUNCTION_BLOCK
