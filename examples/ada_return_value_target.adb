procedure Ada_Return_Value_Target is
   type Pair is record
      X : Integer;
      Y : Integer;
   end record;

   function Return_Pair return Pair is
   begin
      return (7, 11);
   end Return_Pair;

   pragma No_Inline (Return_Pair);
   Result : Pair;
   pragma Volatile (Result);
begin
   Result := Return_Pair;
end Ada_Return_Value_Target;
