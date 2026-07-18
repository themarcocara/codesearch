class Main {
  static function main() {
    var ok = Helper.validate(5);
    var ok2 = Helper.validate(10);
    Sys.println(ok && ok2);
  }
}
