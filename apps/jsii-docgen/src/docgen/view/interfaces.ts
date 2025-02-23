import * as reflect from "jsii-reflect";
import { InterfaceSchema } from "../schema";
import { Transpile } from "../transpile/transpile";
import { Interface } from "./interface";

export class Interfaces {
  private readonly interfaces: Interface[];

  constructor(transpile: Transpile, interfaces: reflect.InterfaceType[]) {
    this.interfaces = interfaces
      .filter((i) => !Interface.isStruct(i))
      .map((i) => new Interface(transpile, i));
  }

  public toJson(): InterfaceSchema[] {
    return this.interfaces.map((iface) => iface.toJson());
  }
}
