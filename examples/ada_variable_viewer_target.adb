procedure Ada_Variable_Viewer_Target is
   type Vector is array (Integer range <>) of Integer;
   type Matrix is array (Integer range <>, Integer range <>) of Integer;
   type Mode is (Idle, Ready);
   type Mode_Counts is array (Mode) of Integer;
   type Particle is record
      Id : Integer;
      Position : Vector (1 .. 3);
   end record;

   Counter : aliased Long_Long_Integer := 42;
   Enabled : Boolean := True;
   Scale : Long_Float := 1.25;
   State : Mode := Ready;
   Counts : Mode_Counts := (Idle => 10, Ready => 20);
   Values : Vector (-2 .. 2) := (-20, -10, 0, 10, 20);
   Grid : Matrix (-1 .. 1, 4 .. 5) := ((1, 2), (3, 4), (5, 6));
   Length : Positive := 5;
   Dynamic : Vector (4 .. Length + 3) := (others => 17);
   Empty : Vector (1 .. 0);
   Message : String := "Hello from Ada";
   Sample : Particle := (7, (1, 2, 3));
   Pointer : access Long_Long_Integer := Counter'Access;
   Missing : access Long_Long_Integer := null;
   Large : Vector (-10 .. 8181);

   procedure Ada_Values_Ready is
   begin
      null;
   end Ada_Values_Ready;

   pragma No_Inline (Ada_Values_Ready);
begin
   for Index in Large'Range loop
      Large (Index) := Index * 3;
   end loop;

   Ada_Values_Ready;
   Counter := Counter + 1;
   Enabled := False;
   Values (0) := 99;
   Ada_Values_Ready;
end Ada_Variable_Viewer_Target;
