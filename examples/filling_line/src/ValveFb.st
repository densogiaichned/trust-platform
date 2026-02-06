FUNCTION_BLOCK ValveFb IMPLEMENTS IValve
VAR
    open_state : BOOL;
END_VAR

METHOD PUBLIC Open : BOOL
    open_state := TRUE;
    Open := open_state;
END_METHOD

METHOD PUBLIC Close : BOOL
    open_state := FALSE;
    Close := open_state;
END_METHOD

PUBLIC PROPERTY IsOpen : BOOL
    GET
        IsOpen := open_state;
    END_GET
END_PROPERTY
END_FUNCTION_BLOCK
